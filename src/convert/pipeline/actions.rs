//! Durable, deterministic pre/post conversion actions.
//!
//! The implementation is deliberately split into four layers:
//! deterministic planning, schema-validated durable journals, a descriptor/capability filesystem seam, and a script-runner trait. Built-in
//! apply and recovery never mutate through an untrusted pathname; absolute
//! paths exist only for planning, preview, and initial root acquisition. Script
//! execution is delegated to a dedicated process-tree supervisor.

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::process::ExitStatus;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use thiserror::Error;
use uuid::Uuid;

use crate::convert::cap_fs::{
    deterministic_scope_id, metadata_for_open_file, CapEntryIdentity, CapFileType, CapFsError,
    CapMetadata,
    CapabilityFilesystem, PinnedDirectoryCapability, RenameNoClobberOutcome, ScopeId,
    ScopeRecord, ScopedPath,
};
use crate::convert::script_supervisor::{
    cleanup_supervised, current_host_boot_identity, local_process_start_identity,
    recover_supervised_with_observer, run_supervised, run_supervised_via_item_supervisor, ContainmentConfidence,
    ContainmentDescriptor, ContainmentPreference, OutputCaptureSummary,
    OutputCaptureTerminal,
    RuntimeDirectoryIdentity, ScriptLifecycleEvent, ScriptRecoveryOutcome, ScriptRecoveryRequest, SupervisedCommand, TerminationReason,
};
#[cfg(test)]
use crate::convert::script_supervisor::{HostBootIdentity, StableProcessIdentity};
use crate::convert::rename_plan::{
    plan_rename_transaction, RenameIntent, RenameTransactionPlan,
};

const JOURNAL_SCHEMA_VERSION: u32 = 9;
const SCRIPT_EXECUTION_SCHEMA_VERSION: u32 = 2;
const CLAIM_SCHEMA_VERSION: u32 = 1;
const RESULT_SCHEMA_VERSION: u32 = 3;
const TERMINAL_JOURNAL_RETENTION_COUNT: usize = 32;
const TERMINAL_JOURNAL_RETENTION_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);
const DEFAULT_SCRIPT_TIMEOUT_SECONDS: u64 = 10 * 60;
const MAX_SCRIPT_TIMEOUT_SECONDS: u64 = 24 * 60 * 60;
const INTERNAL_WORKSPACE_PREFIX: &str = ".tonepoet-actions-";
const GENERATED_CONVERSION_LOG: &str = "conversion.log";
const EXPLICIT_ACTIVE_RUN_SCHEMA_VERSION: u32 = 1;
const EXPLICIT_PREVIEW_SCHEMA_VERSION: u32 = 5;
const EXPLICIT_ACTIVE_RUN_FILE: &str = ".active-run.json";
const EXPLICIT_ACTIVE_RUN_TEMP_FILE: &str = ".active-run.write.tmp";
const EXPLICIT_PREVIEW_FILE: &str = ".preview-authority.json";
const EXPLICIT_PREVIEW_TEMP_FILE: &str = ".preview-authority.write.tmp";

#[cfg(test)]
thread_local! {
    /// Per-test-thread journal persistence fault injection. Rust's test
    /// harness executes tests concurrently, so a process-global counter would
    /// make unrelated recovery tests interfere with one another.
    static TEST_JOURNAL_PERSIST_FAULT: std::cell::Cell<(usize, usize)> =
        const { std::cell::Cell::new((0, 0)) };
}

#[cfg(test)]
fn test_set_journal_persist_fault(fail_at: Option<usize>) {
    TEST_JOURNAL_PERSIST_FAULT.with(|fault| fault.set((fail_at.unwrap_or(0), 0)));
}

#[cfg(test)]
fn test_maybe_fail_journal_persist() -> Result<(), ActionError> {
    TEST_JOURNAL_PERSIST_FAULT.with(|fault| {
        let (fail_at, calls) = fault.get();
        if fail_at == 0 {
            return Ok(());
        }
        let calls = calls.saturating_add(1);
        if calls == fail_at {
            // Fail once. Recovery code in the same test thread must be able to
            // continue without depending on every caller remembering to clear
            // the hook before it performs cleanup.
            fault.set((0, calls));
            return Err(ActionError::Io(io::Error::new(
                io::ErrorKind::Other,
                format!("injected journal persistence failure at call {calls}"),
            )));
        }
        fault.set((fail_at, calls));
        Ok(())
    })
}


#[cfg(test)]
#[derive(Debug)]
struct DurableReportPauseHook {
    claimed: std::sync::atomic::AtomicBool,
    entered: std::sync::mpsc::Sender<()>,
    resume: Mutex<std::sync::mpsc::Receiver<()>>,
}

#[cfg(test)]
static DURABLE_REPORT_PAUSE_HOOKS: OnceLock<
    Mutex<BTreeMap<PathBuf, std::sync::Weak<DurableReportPauseHook>>>,
> = OnceLock::new();

/// Per-journal deterministic test control for pausing exactly one durable
/// report reader after the journal has been loaded but before capability scope
/// restoration/authentication. The registry is weak and keyed by the unique
/// journal path, so parallel tests do not serialize one another and dropping
/// the control reclaims the entry.
#[cfg(test)]
pub(crate) struct DurableReportPauseControl {
    journal_path: PathBuf,
    hook: Arc<DurableReportPauseHook>,
    entered: Mutex<std::sync::mpsc::Receiver<()>>,
    resume: std::sync::mpsc::Sender<()>,
}

#[cfg(test)]
impl DurableReportPauseControl {
    pub(crate) fn wait_until_paused(&self) {
        self.entered
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .recv_timeout(Duration::from_secs(10))
            .expect("durable report reader did not reach the installed journal-load pause");
    }

    pub(crate) fn resume(&self) {
        self.resume
            .send(())
            .expect("paused durable report reader disappeared before resume");
    }
}

#[cfg(test)]
impl Drop for DurableReportPauseControl {
    fn drop(&mut self) {
        let Some(registry) = DURABLE_REPORT_PAUSE_HOOKS.get() else {
            return;
        };
        let Ok(mut registry) = registry.lock() else {
            return;
        };
        let hook = Arc::downgrade(&self.hook);
        if registry
            .get(&self.journal_path)
            .is_some_and(|registered| registered.ptr_eq(&hook))
        {
            registry.remove(&self.journal_path);
        }
        registry.retain(|_, registered| registered.strong_count() > 0);
    }
}

#[cfg(test)]
pub(crate) fn install_durable_report_pause_after_journal_load(
    pipeline: &ActionPipeline,
    context: &ActionContext,
) -> Result<DurableReportPauseControl, ActionError> {
    let pipeline_serialized = pipeline.canonical_serialization()?;
    let pipeline_sha256 = sha256_hex(pipeline_serialized.as_bytes());
    let journal_path = action_journal_path(context, &pipeline_sha256)?;
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (resume_tx, resume_rx) = std::sync::mpsc::channel();
    let hook = Arc::new(DurableReportPauseHook {
        claimed: std::sync::atomic::AtomicBool::new(false),
        entered: entered_tx,
        resume: Mutex::new(resume_rx),
    });
    let registry = DURABLE_REPORT_PAUSE_HOOKS
        .get_or_init(|| Mutex::new(BTreeMap::new()));
    let mut registry = registry.lock().map_err(|_| {
        ActionError::Contradiction(
            "durable-report pause-hook registry mutex poisoned".to_string(),
        )
    })?;
    registry.retain(|_, registered| registered.strong_count() > 0);
    if registry
        .get(&journal_path)
        .and_then(std::sync::Weak::upgrade)
        .is_some()
    {
        return Err(ActionError::Contradiction(format!(
            "a durable-report pause hook is already installed for {}",
            journal_path.display()
        )));
    }
    registry.insert(journal_path.clone(), Arc::downgrade(&hook));
    Ok(DurableReportPauseControl {
        journal_path,
        hook,
        entered: Mutex::new(entered_rx),
        resume: resume_tx,
    })
}

#[cfg(test)]
fn test_pause_durable_report_after_journal_load(
    journal_path: &Path,
) -> Result<(), ActionError> {
    let hook = DURABLE_REPORT_PAUSE_HOOKS
        .get()
        .and_then(|registry| registry.lock().ok())
        .and_then(|mut registry| {
            registry.retain(|_, registered| registered.strong_count() > 0);
            registry.get(journal_path).and_then(std::sync::Weak::upgrade)
        });
    let Some(hook) = hook else {
        return Ok(());
    };
    if hook
        .claimed
        .swap(true, std::sync::atomic::Ordering::SeqCst)
    {
        return Ok(());
    }
    hook.entered.send(()).map_err(|_| {
        ActionError::Contradiction(
            "durable-report pause controller disappeared before journal validation".to_string(),
        )
    })?;
    hook.resume
        .lock()
        .map_err(|_| {
            ActionError::Contradiction(
                "durable-report pause receiver mutex poisoned".to_string(),
            )
        })?
        .recv_timeout(Duration::from_secs(10))
        .map_err(|error| {
            ActionError::Contradiction(format!(
                "durable-report pause did not receive a deterministic resume signal: {error}"
            ))
        })?;
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

// ---------------------------------------------------------------------------
// Public model and persistence
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionPhase {
    Pre,
    Post,
}

impl ActionPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pre => "pre",
            Self::Post => "post",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ActionPipeline {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pre: Vec<ConversionAction>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub post: Vec<ConversionAction>,
}

impl ActionPipeline {
    pub fn for_phase(&self, phase: ActionPhase) -> &[ConversionAction] {
        match phase {
            ActionPhase::Pre => &self.pre,
            ActionPhase::Post => &self.post,
        }
    }

    pub fn for_phase_mut(&mut self, phase: ActionPhase) -> &mut Vec<ConversionAction> {
        match phase {
            ActionPhase::Pre => &mut self.pre,
            ActionPhase::Post => &mut self.post,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.pre.is_empty() && self.post.is_empty()
    }

    pub fn canonical_serialization(&self) -> Result<String, ActionError> {
        serde_json::to_string(self).map_err(ActionError::Serialization)
    }

    pub fn canonical_sha256(&self) -> Result<String, ActionError> {
        Ok(sha256_hex(self.canonical_serialization()?.as_bytes()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConversionAction {
    Rename(RenameAction),
    Copy(CopyAction),
    Move(MoveAction),
    Delete(DeleteAction),
    CreateFolder(CreateFolderAction),
    Runscript(RunScriptAction),
}

impl ConversionAction {
    pub fn continue_on_error(&self) -> bool {
        match self {
            Self::Rename(action) => action.targeting.continue_on_error,
            Self::Copy(action) => action.targeting.continue_on_error,
            Self::Move(action) => action.targeting.continue_on_error,
            Self::Delete(action) => action.targeting.continue_on_error,
            Self::CreateFolder(action) => action.continue_on_error,
            Self::Runscript(action) => action.continue_on_error,
        }
    }

    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::Rename(_) => "rename",
            Self::Copy(_) => "copy",
            Self::Move(_) => "move",
            Self::Delete(_) => "delete",
            Self::CreateFolder(_) => "create_folder",
            Self::Runscript(_) => "runscript",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetSpec {
    #[serde(default)]
    pub target: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default)]
    pub allow_sources: bool,
    #[serde(default)]
    pub continue_on_error: bool,
}

impl Default for TargetSpec {
    fn default() -> Self {
        Self {
            target: Vec::new(),
            exclude: Vec::new(),
            allow_sources: false,
            continue_on_error: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenameMode {
    Template,
    Uppercase,
    Lowercase,
    Fixcaps,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenameAction {
    #[serde(flatten)]
    pub targeting: TargetSpec,
    pub mode: RenameMode,
    #[serde(default)]
    pub template: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CopyAction {
    #[serde(flatten)]
    pub targeting: TargetSpec,
    pub destination: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MoveAction {
    #[serde(flatten)]
    pub targeting: TargetSpec,
    pub destination: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteAction {
    #[serde(flatten)]
    pub targeting: TargetSpec,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateFolderAction {
    pub path: PathBuf,
    #[serde(default)]
    pub continue_on_error: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunScriptAction {
    pub script: PathBuf,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default = "default_script_timeout_seconds")]
    pub timeout_seconds: u64,
    #[serde(default)]
    pub continue_on_error: bool,
}

fn default_script_timeout_seconds() -> u64 {
    DEFAULT_SCRIPT_TIMEOUT_SECONDS
}

// ---------------------------------------------------------------------------
// Canonical semantic seams supplied by the pipeline
// ---------------------------------------------------------------------------

pub type WildcardMatcher = fn(&str, &str) -> bool;
pub type TemplateRenderer = fn(&str, &BTreeMap<String, String>) -> Result<String, String>;
pub type ComponentSanitizer = fn(&str) -> String;
pub type FixCapsRenderer = fn(&str) -> String;
pub type DiscNumberResolver = fn(&Path) -> Option<u32>;

#[derive(Clone)]
pub struct ActionSemantics {
    pub wildcard_matches: WildcardMatcher,
    pub render_template: TemplateRenderer,
    pub sanitize_component: ComponentSanitizer,
    pub fixcaps: FixCapsRenderer,
    pub disc_number_for_path: DiscNumberResolver,
}

impl std::fmt::Debug for ActionSemantics {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ActionSemantics")
            .field("wildcard_matches", &"<pipeline matcher>")
            .field("render_template", &"<pipeline renderer>")
            .field("sanitize_component", &"<pipeline sanitizer>")
            .field("fixcaps", &"<conversion capitalization>")
            .field("disc_number_for_path", &"<pipeline disc evidence>")
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct ActionContext {
    pub run_identity: String,
    pub album_identity: String,
    pub phase: ActionPhase,
    pub subject_dir: PathBuf,
    pub source_path: PathBuf,
    pub source_is_directory: bool,
    pub output_root: PathBuf,
    pub album_dir: PathBuf,
    /// Stable user-facing album path requested for script export. Post-action
    /// supervisors rebind this value to a pathname verified against the
    /// retained working-directory descriptor immediately before execution.
    pub environment_album_dir: Option<PathBuf>,
    /// Runtime-only descriptor authority for the exact post-publication album
    /// object. Journals continue to store `album_dir` as the stable logical
    /// path; capability I/O is injected from this retained descriptor.
    pub retained_album_capability: Option<Arc<PinnedDirectoryCapability>>,
    /// Runtime-only descriptor authority for the output/coordination root.
    /// This binds journals and other post-action state to the same filesystem
    /// object as publication without persisting descriptor namespace paths.
    pub retained_output_capability: Option<Arc<PinnedDirectoryCapability>>,
    /// Runtime-only exact descriptor for the durable journal directory.
    pub retained_journal_capability: Option<Arc<PinnedDirectoryCapability>>,
    /// Ephemeral descriptor-relative route used only for election files and
    /// short-lived coordination locks. It is never serialized into a journal.
    pub coordination_io_dir: Option<PathBuf>,
    pub protected_sources: BTreeSet<PathBuf>,
    pub protected_generated_paths: BTreeSet<PathBuf>,
    pub album_tokens: BTreeMap<String, String>,
    pub disc_count: Option<u32>,
    pub journal_dir: PathBuf,
    /// Album-batch source grouping root. When present, subject/source
    /// capability scopes anchor here so every participant of the batch pins
    /// IDENTICAL scope roots regardless of which disc subdirectory its own
    /// track lives in — a per-track anchor would make the elected runner's
    /// journal unvalidatable by participants from sibling disc directories.
    pub batch_source_scope_root: Option<PathBuf>,
    pub explicit_scope: bool,
    pub semantics: ActionSemantics,
}

impl ActionContext {
    pub fn tokens_for_path(&self, path: &Path) -> BTreeMap<String, String> {
        let mut tokens = self.album_tokens.clone();
        let disc = (self.semantics.disc_number_for_path)(path);
        insert_disc_tokens(&mut tokens, disc, self.disc_count);
        tokens.insert(
            "FILENAME".to_string(),
            path.file_name()
                .map(|value| value.to_string_lossy().to_string())
                .unwrap_or_default(),
        );
        tokens.insert(
            "STEM".to_string(),
            path.file_stem()
                .map(|value| value.to_string_lossy().to_string())
                .unwrap_or_default(),
        );
        tokens.insert(
            "EXT".to_string(),
            path.extension()
                .map(|value| value.to_string_lossy().to_string())
                .unwrap_or_default(),
        );
        tokens
    }
}

fn insert_disc_tokens(
    tokens: &mut BTreeMap<String, String>,
    disc_number: Option<u32>,
    disc_count: Option<u32>,
) {
    let emit = disc_count.unwrap_or(1) > 1;
    let value = if emit { disc_number } else { None };
    tokens.insert(
        "DISCNUMBER".to_string(),
        value.map(|number| number.to_string()).unwrap_or_default(),
    );
    tokens.insert(
        "NNDISCNUMBER".to_string(),
        value.map(|number| format!("{number:02}")).unwrap_or_default(),
    );
    tokens.insert(
        "NNNDISCNUMBER".to_string(),
        value.map(|number| format!("{number:03}")).unwrap_or_default(),
    );
}

// ---------------------------------------------------------------------------
// Plan and report model
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ActionPlan {
    pub action_kind: String,
    #[serde(default)]
    pub operations: Vec<PlannedOperation>,
    /// Durable facts that justified state-dependent planner decisions which
    /// emitted no mutation. Exact-preview execution revalidates these facts;
    /// a zero-operation plan is never treated as authority by itself.
    #[serde(default)]
    pub planning_preconditions: Vec<PlanningPrecondition>,
    #[serde(default)]
    pub notices: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "precondition", rename_all = "snake_case")]
pub enum PlanningPrecondition {
    CopyAlreadyEquivalent {
        source: PathBuf,
        destination: PathBuf,
        expected_source: ObjectIdentity,
        expected_destination: ObjectIdentity,
    },
    DirectoryAlreadyExists {
        path: PathBuf,
        expected_directory: ObjectIdentity,
    },
    RenameAlreadyNamed {
        path: PathBuf,
        expected_entry: ObjectIdentity,
    },
    MoveAlreadyAtDestination {
        path: PathBuf,
        expected_entry: ObjectIdentity,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum PlannedOperation {
    Rename {
        source: PathBuf,
        destination: PathBuf,
        staging: PathBuf,
        expected_source: ObjectIdentity,
        /// Expected content after all selected descendants have been detached
        /// into their own staging paths. Equal to `expected_source` for files
        /// and non-nested directory selections.
        expected_staged: ObjectIdentity,
    },
    Copy {
        source: PathBuf,
        destination: PathBuf,
        temporary: PathBuf,
        publication_witness: PathBuf,
        expected_source: ObjectIdentity,
    },
    RepairCopyMetadata {
        source: PathBuf,
        destination: PathBuf,
        expected_source: ObjectIdentity,
        expected_destination: ObjectIdentity,
        include_hidden: bool,
    },
    Move {
        source: PathBuf,
        destination: PathBuf,
        temporary: PathBuf,
        publication_witness: PathBuf,
        source_witness: PathBuf,
        expected_source: ObjectIdentity,
    },
    Delete {
        target: PathBuf,
        witness: PathBuf,
        expected_target: ObjectIdentity,
    },
    CreateDirectory {
        path: PathBuf,
    },
    RunScript {
        script: PathBuf,
        expected_script: ObjectIdentity,
        args: Vec<String>,
        working_directory: PathBuf,
        environment: BTreeMap<String, String>,
        timeout_seconds: u64,
        runtime_directory: PathBuf,
        containment_token: String,
    },
}

/// Resolve the shared cross-session mutation admission set for concrete action
/// plans. Action journals/capabilities remain the transaction authority; these
/// claims only make other tonepoet mutation subsystems observe the same paths.
pub fn shared_path_claims_for_action_plans(
    plans: &[ActionPlan],
    context: &ActionContext,
) -> Result<Vec<crate::concurrency::PathClaim>, ActionError> {
    use crate::concurrency::{ClaimMode, ClaimScope, PathClaim};
    fn scope(path: &Path) -> ClaimScope {
        if std::fs::metadata(path).is_ok_and(|metadata| metadata.is_dir()) {
            ClaimScope::Subtree
        } else {
            ClaimScope::Exact
        }
    }
    let mut claims = Vec::new();
    let mut has_script = false;
    for plan in plans {
        for operation in &plan.operations {
            let mut add = |path: &Path, mode: ClaimMode, claim_scope: ClaimScope| -> Result<(), ActionError> {
                claims.push(PathClaim::resolve(path, mode, claim_scope).map_err(ActionError::Conflict)?);
                Ok(())
            };
            match operation {
                PlannedOperation::Rename { source, destination, .. } => {
                    add(source, ClaimMode::Write, scope(source))?;
                    add(destination, ClaimMode::Write, scope(destination))?;
                }
                PlannedOperation::Copy { source, destination, .. } => {
                    add(source, ClaimMode::Read, scope(source))?;
                    add(destination, ClaimMode::Write, scope(destination))?;
                }
                PlannedOperation::RepairCopyMetadata { source, destination, .. } => {
                    add(source, ClaimMode::Read, scope(source))?;
                    add(destination, ClaimMode::Write, scope(destination))?;
                }
                PlannedOperation::Move { source, destination, .. } => {
                    add(source, ClaimMode::Write, scope(source))?;
                    add(destination, ClaimMode::Write, scope(destination))?;
                }
                PlannedOperation::Delete { target, .. } => {
                    add(target, ClaimMode::Write, scope(target))?;
                }
                PlannedOperation::CreateDirectory { path } => {
                    add(path, ClaimMode::Write, ClaimScope::Subtree)?;
                }
                PlannedOperation::RunScript { .. } => has_script = true,
            }
        }
    }
    if has_script {
        // Arbitrary script side effects outside tonepoet's known output roots
        // are intentionally not guessed. The two namespaces explicitly handed
        // to the script are, however, admitted before the exec gate can open.
        claims.push(PathClaim::resolve(
            &context.album_dir,
            ClaimMode::Write,
            ClaimScope::Subtree,
        ).map_err(ActionError::Conflict)?);
        claims.push(PathClaim::resolve(
            &context.output_root,
            ClaimMode::Write,
            ClaimScope::Subtree,
        ).map_err(ActionError::Conflict)?);
    }
    Ok(claims)
}

fn configured_target_phase_claims(
    spec: &TargetSpec,
    context: &ActionContext,
    mode: crate::concurrency::ClaimMode,
) -> Result<Vec<crate::concurrency::PathClaim>, ActionError> {
    use crate::concurrency::{ClaimScope, PathClaim};

    validate_target_patterns(spec)?;
    if spec.target.iter().any(|pattern| contains_wildcard(pattern)) {
        return Ok(vec![PathClaim::resolve(
            &context.subject_dir,
            mode,
            ClaimScope::Subtree,
        )
        .map_err(ActionError::Conflict)?]);
    }

    let mut claims = Vec::new();
    for pattern in &spec.target {
        let relative = checked_relative_target(pattern)?;
        let path = context.subject_dir.join(relative);
        let scope = if std::fs::metadata(&path).is_ok_and(|metadata| metadata.is_dir()) {
            ClaimScope::Subtree
        } else {
            ClaimScope::Exact
        };
        claims.push(PathClaim::resolve(&path, mode, scope).map_err(ActionError::Conflict)?);
    }
    Ok(claims)
}

/// Derive the complete cross-session capability for one configured automatic
/// action phase without enumerating wildcard matches or eagerly planning every
/// leaf operation. Destination configuration already supplies the stable roots
/// needed for atomic admission.
fn shared_path_claims_for_configured_action_phase(
    pipeline: &ActionPipeline,
    context: &ActionContext,
) -> Result<Vec<crate::concurrency::PathClaim>, ActionError> {
    use crate::concurrency::{ClaimMode, ClaimScope, PathClaim};

    let mut claims = Vec::new();
    for action in pipeline.for_phase(context.phase) {
        validate_phase_action(action, context)?;
        match action {
            ConversionAction::Rename(action) => {
                validate_target_patterns(&action.targeting)?;
                if action.targeting.target.iter().any(|pattern| contains_wildcard(pattern)) {
                    // A wildcard rename can select any descendant and always
                    // publishes its renamed entry back into the same subject tree.
                    claims.push(
                        PathClaim::resolve(&context.subject_dir, ClaimMode::Write, ClaimScope::Subtree)
                            .map_err(ActionError::Conflict)?,
                    );
                } else {
                    for pattern in &action.targeting.target {
                        let source = context.subject_dir.join(checked_relative_target(pattern)?);
                        let is_directory =
                            std::fs::metadata(&source).is_ok_and(|metadata| metadata.is_dir());
                        let scope = if is_directory {
                            ClaimScope::Subtree
                        } else {
                            ClaimScope::Exact
                        };
                        let destination =
                            rename_destination_for_kind(action, context, &source, is_directory)?;
                        claims.push(
                            PathClaim::resolve(&source, ClaimMode::Write, scope)
                                .map_err(ActionError::Conflict)?,
                        );
                        claims.push(
                            PathClaim::resolve(&destination, ClaimMode::Write, scope)
                                .map_err(ActionError::Conflict)?,
                        );
                    }
                }
            }
            ConversionAction::Copy(action) => {
                claims.extend(configured_target_phase_claims(
                    &action.targeting,
                    context,
                    ClaimMode::Read,
                )?);
                let destination =
                    render_action_path(&action.destination, context, &context.subject_dir)?;
                claims.push(
                    PathClaim::resolve(&destination, ClaimMode::Write, ClaimScope::Subtree)
                        .map_err(ActionError::Conflict)?,
                );
            }
            ConversionAction::Move(action) => {
                claims.extend(configured_target_phase_claims(
                    &action.targeting,
                    context,
                    ClaimMode::Write,
                )?);
                let destination =
                    render_action_path(&action.destination, context, &context.subject_dir)?;
                claims.push(
                    PathClaim::resolve(&destination, ClaimMode::Write, ClaimScope::Subtree)
                        .map_err(ActionError::Conflict)?,
                );
            }
            ConversionAction::Delete(action) => {
                claims.extend(configured_target_phase_claims(
                    &action.targeting,
                    context,
                    ClaimMode::Write,
                )?);
            }
            ConversionAction::CreateFolder(action) => {
                let path = render_action_path(&action.path, context, &context.subject_dir)?;
                claims.push(
                    PathClaim::resolve(&path, ClaimMode::Write, ClaimScope::Subtree)
                        .map_err(ActionError::Conflict)?,
                );
            }
            ConversionAction::Runscript(_) => {
                // User code can have arbitrary effects; only claim the managed
                // Tonepoet namespaces explicitly exposed to the script.
                claims.push(
                    PathClaim::resolve(&context.album_dir, ClaimMode::Write, ClaimScope::Subtree)
                        .map_err(ActionError::Conflict)?,
                );
                claims.push(
                    PathClaim::resolve(&context.output_root, ClaimMode::Write, ClaimScope::Subtree)
                        .map_err(ActionError::Conflict)?,
                );
            }
        }
    }
    Ok(claims)
}

fn remove_covered_phase_claims(
    candidates: Vec<crate::concurrency::PathClaim>,
    already_held: &[crate::concurrency::PathClaim],
) -> Vec<crate::concurrency::PathClaim> {
    let mut retained: Vec<crate::concurrency::PathClaim> = Vec::new();
    for candidate in candidates {
        if already_held.iter().any(|claim| claim.covers(&candidate))
            || retained.iter().any(|claim| claim.covers(&candidate))
        {
            continue;
        }
        retained.retain(|claim| !candidate.covers(claim));
        retained.push(candidate);
    }
    retained
}

/// Atomically admit the complete configured mutation set for an automatic
/// conversion action phase. The lease is registered before the phase loop, so
/// the item supervisor receives it before any mutation-capable script/tool can
/// cross its execution gate. Manual reviewed `:actions-run` invocations keep
/// their existing outer EphemeralMutation admission and do not use this path.
fn admit_conversion_action_phase_claims(
    pipeline: &ActionPipeline,
    context: &ActionContext,
    prepared_explicit: bool,
) -> Result<(), ActionError> {
    if prepared_explicit {
        return Ok(());
    }
    let Some(item_id) = crate::concurrency::current_execution_item() else {
        return Ok(());
    };
    let Some(execution_id) = crate::concurrency::runtime_execution_id(&item_id) else {
        return Err(ActionError::Conflict(format!(
            "conversion action lost QueueExecution authority for item {item_id}"
        )));
    };
    let already_held =
        crate::concurrency::runtime_execution_claims(&item_id).map_err(ActionError::Conflict)?;
    let claims = remove_covered_phase_claims(
        shared_path_claims_for_configured_action_phase(pipeline, context)?,
        &already_held,
    );
    if claims.is_empty() {
        return Ok(());
    }
    let lease = crate::concurrency::MutationClaimGuard::acquire(
        crate::concurrency::LeaseFamily::ExecutionClaim { execution_id },
        claims,
    )
    .map_err(ActionError::Conflict)?
    .into_lease();
    crate::concurrency::register_runtime_supplemental_lease(&item_id, Arc::new(lease))
        .map_err(ActionError::Conflict)
}

fn assert_conversion_action_plan_is_admitted(
    plan: &ActionPlan,
    context: &ActionContext,
    prepared_explicit: bool,
) -> Result<(), ActionError> {
    if prepared_explicit {
        return Ok(());
    }
    let Some(item_id) = crate::concurrency::current_execution_item() else {
        return Ok(());
    };
    let admitted =
        crate::concurrency::runtime_execution_claims(&item_id).map_err(ActionError::Conflict)?;
    for required in shared_path_claims_for_action_plans(std::slice::from_ref(plan), context)? {
        if !admitted.iter().any(|claim| claim.covers(&required)) {
            return Err(ActionError::Contradiction(format!(
                "automatic action plan escaped phase mutation capability: {}",
                required.identity.original.display()
            )));
        }
    }
    Ok(())
}

/// Human-readable dry-run lines for one planned action: one line per
/// operation, then plan notices. Scripts render as "would run: …" — preview
/// never executes (SR-6).
pub fn describe_plan(plan: &ActionPlan) -> Vec<String> {
    let mut lines = Vec::new();
    for operation in &plan.operations {
        lines.push(match operation {
            PlannedOperation::Rename {
                source,
                destination,
                ..
            } => format!("rename {} -> {}", source.display(), destination.display()),
            PlannedOperation::Copy {
                source,
                destination,
                ..
            } => format!("copy {} -> {}", source.display(), destination.display()),
            PlannedOperation::RepairCopyMetadata {
                source,
                destination,
                ..
            } => format!(
                "repair copied metadata {} -> {}",
                source.display(),
                destination.display()
            ),
            PlannedOperation::Move {
                source,
                destination,
                ..
            } => format!("move {} -> {}", source.display(), destination.display()),
            PlannedOperation::Delete { target, .. } => {
                format!("delete {}", target.display())
            }
            PlannedOperation::CreateDirectory { path } => {
                format!("create folder {}", path.display())
            }
            PlannedOperation::RunScript {
                script,
                args,
                timeout_seconds,
                ..
            } => {
                if args.is_empty() {
                    format!(
                        "would run: {} (timeout {timeout_seconds}s)",
                        script.display()
                    )
                } else {
                    format!(
                        "would run: {} {} (timeout {timeout_seconds}s)",
                        script.display(),
                        args.join(" ")
                    )
                }
            }
        });
    }
    if plan.operations.is_empty() {
        lines.push("no operations — target state already holds".to_string());
    }
    lines.extend(plan.notices.iter().cloned());
    lines
}

impl PlannedOperation {
    fn kind(&self) -> OperationKind {
        match self {
            Self::Rename { .. } => OperationKind::Rename,
            Self::Copy { .. } => OperationKind::Copy,
            Self::RepairCopyMetadata { .. } => OperationKind::CopyMetadataRepair,
            Self::Move { .. } => OperationKind::Move,
            Self::Delete { .. } => OperationKind::Delete,
            Self::CreateDirectory { .. } => OperationKind::CreateDirectory,
            Self::RunScript { .. } => OperationKind::RunScript,
        }
    }

    fn all_paths(&self) -> Vec<&Path> {
        match self {
            Self::Rename {
                source,
                destination,
                staging,
                ..
            } => vec![source, destination, staging],
            Self::Copy {
                source,
                destination,
                temporary,
                publication_witness,
                ..
            } => vec![source, destination, temporary, publication_witness],
            Self::RepairCopyMetadata {
                source,
                destination,
                ..
            } => vec![source, destination],
            Self::Move {
                source,
                destination,
                temporary,
                publication_witness,
                source_witness,
                ..
            } => vec![
                source,
                destination,
                temporary,
                publication_witness,
                source_witness,
            ],
            Self::Delete {
                target, witness, ..
            } => vec![target, witness],
            Self::CreateDirectory { path } => vec![path],
            Self::RunScript {
                script,
                working_directory,
                runtime_directory,
                ..
            } => vec![script, working_directory, runtime_directory],
        }
    }
}

fn operation_summary(operation: &PlannedOperation) -> String {
    match operation {
        PlannedOperation::Rename { source, destination, .. } => {
            format!("rename {} -> {}", source.display(), destination.display())
        }
        PlannedOperation::Copy { source, destination, .. } => {
            format!("copy {} -> {}", source.display(), destination.display())
        }
        PlannedOperation::RepairCopyMetadata { source, destination, .. } => {
            format!("repair copied metadata {} -> {}", source.display(), destination.display())
        }
        PlannedOperation::Move { source, destination, .. } => {
            format!("move {} -> {}", source.display(), destination.display())
        }
        PlannedOperation::Delete { target, .. } => format!("delete {}", target.display()),
        PlannedOperation::CreateDirectory { path } => {
            format!("create folder {}", path.display())
        }
        PlannedOperation::RunScript { script, .. } => {
            format!("run script {}", script.display())
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ActionPhaseReport {
    pub phase: Option<ActionPhase>,
    #[serde(default)]
    pub actions: Vec<ActionResult>,
    #[serde(default)]
    pub notices: Vec<String>,
    #[serde(default)]
    pub recovery_required: bool,
    #[serde(default)]
    pub cancelled: bool,
}

impl ActionPhaseReport {
    pub fn has_errors(&self) -> bool {
        self.actions.iter().any(|action| {
            matches!(
                action.status,
                ActionResultStatus::Failed
                    | ActionResultStatus::Interrupted
                    | ActionResultStatus::ManualRecoveryRequired
            )
        })
    }

    pub fn operation_count(&self) -> usize {
        self.actions
            .iter()
            .map(|action| action.operations.len())
            .sum()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionResult {
    pub index: usize,
    pub kind: String,
    pub status: ActionResultStatus,
    #[serde(default)]
    pub operations: Vec<OperationResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default)]
    pub notices: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionResultStatus {
    Completed,
    NoOp,
    Failed,
    SkippedAfterFailure,
    CancelledBeforeMutation,
    Interrupted,
    ManualRecoveryRequired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationResult {
    pub operation_id: String,
    pub summary: String,
    pub status: OperationResultStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout_tail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr_tail: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationResultStatus {
    Completed,
    Failed,
    Skipped,
    Interrupted,
    ManualRecoveryRequired,
}

#[derive(Debug, Error)]
pub enum ActionError {
    #[error("action I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("action serialization failed: {0}")]
    Serialization(serde_json::Error),
    #[error("unsafe action path: {0}")]
    UnsafePath(String),
    #[error("action plan conflict: {0}")]
    Conflict(String),
    #[error("action preview is stale; refresh required: {0}")]
    PreviewStale(String),
    #[error("action journal is invalid: {0}")]
    InvalidJournal(String),
    #[error("action recovery contradiction: {0}")]
    Contradiction(String),
    #[error("script action failed: {0}")]
    Script(String),
    #[error("action cancelled before mutation: {0}")]
    CancelledBeforeMutation(String),
    #[error("action interrupted after mutation; recovery remains required: {0}")]
    Interrupted(String),
    #[error("manual recovery required: {0}")]
    ManualRecoveryRequired(String),
    #[error("action election failed closed: {0}")]
    Election(String),
}


impl From<CapFsError> for ActionError {
    fn from(error: CapFsError) -> Self {
        match error {
            CapFsError::InvalidPath(message)
            | CapFsError::OutsideScope(message)
            | CapFsError::UnsupportedObject(message) => Self::UnsafePath(message),
            CapFsError::AlreadyExists(message) => Self::Conflict(message),
            CapFsError::ScopeConflict(message)
            | CapFsError::NoClobberUnavailable(message)
            | CapFsError::Contradiction(message) => Self::Contradiction(message),
            CapFsError::Io(error) => Self::Io(error),
        }
    }
}

impl ActionError {
    fn deterministic(&self) -> bool {
        matches!(
            self,
            Self::UnsafePath(_)
                | Self::Conflict(_)
                | Self::PreviewStale(_)
                | Self::Script(_)
                | Self::CancelledBeforeMutation(_)
        )
    }
}

pub trait ActionCancellation: Send + Sync {
    fn is_cancelled(&self) -> bool;
}

/// Coarse-grained progress for potentially expensive explicit-preview
/// preparation. Implementations must remain non-blocking; preparation runs on
/// a worker thread and cancellation is checked independently.
pub trait ExplicitPreviewProgressObserver: Send + Sync {
    fn update(&self, phase: &'static str, completed: u64, total: Option<u64>);
}

#[derive(Debug, Default)]
pub struct NoExplicitPreviewProgress;

impl ExplicitPreviewProgressObserver for NoExplicitPreviewProgress {
    fn update(&self, _phase: &'static str, _completed: u64, _total: Option<u64>) {}
}

#[derive(Debug, Default)]
pub struct NeverCancel;

impl ActionCancellation for NeverCancel {
    fn is_cancelled(&self) -> bool {
        false
    }
}

// ---------------------------------------------------------------------------
// Filesystem identity and mutation seam
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectKind {
    File,
    Directory,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectIdentity {
    pub kind: ObjectKind,
    pub content_sha256: String,
    pub byte_length: u64,
    pub entry_count: u64,
    /// Metadata promised by the copy contract. Directory identities contain
    /// one deterministic record for the root and every copied descendant.
    pub copy_metadata: CopyMetadataIdentity,
    pub filesystem: FilesystemIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CopyMetadataIdentity {
    pub root: CopyMetadataEntry,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub descendants: Vec<CopyMetadataEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CopyMetadataEntry {
    pub relative_path: PathBuf,
    pub kind: ObjectKind,
    pub mode: u32,
    pub modified_nanos: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilesystemIdentity {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inode: Option<u64>,
    pub length: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_nanos: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed_nanos: Option<i64>,
}

impl ObjectIdentity {
    fn same_content(&self, other: &Self) -> bool {
        self.kind == other.kind
            && self.content_sha256 == other.content_sha256
            && self.byte_length == other.byte_length
            && self.entry_count == other.entry_count
    }

    /// Canonical copy/rerun/recovery equivalence. This is intentionally
    /// stricter than byte identity: every promised mode and modification time
    /// must match, with the same one-second timestamp tolerance used by the
    /// companion-copy path for filesystems with coarse timestamp resolution.
    fn copy_state_equivalent(&self, other: &Self) -> bool {
        self.same_content(other)
            && copy_metadata_equivalent(&self.copy_metadata, &other.copy_metadata)
    }

    fn same_object(&self, other: &Self) -> bool {
        self.same_content(other) && self.filesystem == other.filesystem
    }
}

const COPY_MTIME_TOLERANCE_NANOS: i64 = 1_000_000_000;

fn copy_metadata_equivalent(left: &CopyMetadataIdentity, right: &CopyMetadataIdentity) -> bool {
    copy_metadata_entry_equivalent(&left.root, &right.root)
        && left.descendants.len() == right.descendants.len()
        && left
            .descendants
            .iter()
            .zip(&right.descendants)
            .all(|(left, right)| copy_metadata_entry_equivalent(left, right))
}

fn copy_metadata_entry_equivalent(left: &CopyMetadataEntry, right: &CopyMetadataEntry) -> bool {
    left.relative_path == right.relative_path
        && left.kind == right.kind
        && left.mode == right.mode
        && left.modified_nanos.abs_diff(right.modified_nanos)
            <= COPY_MTIME_TOLERANCE_NANOS as u64
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionEntryType {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveRenameAttempt {
    Renamed,
    CrossDevice,
}

/// Descriptor-retention verification: compare replacement/change attributes
/// only — reading the retained descriptor may legitimately update atime, and
/// the digest is the authoritative content check.
fn script_verification_metadata_matches(before: CapMetadata, after: CapMetadata) -> bool {
    before.file_type == after.file_type
        && before.device == after.device
        && before.inode == after.inode
        && before.length == after.length
        && before.mode == after.mode
        && before.modified_seconds == after.modified_seconds
        && before.modified_nanos == after.modified_nanos
        && before.changed_seconds == after.changed_seconds
        && before.changed_nanos == after.changed_nanos
}

pub trait ActionFilesystem: Send + Sync {
    fn pin_root(&self, id: ScopeId, path: &Path) -> Result<(), ActionError>;
    fn pin_materializable_root(&self, id: ScopeId, path: &Path) -> Result<(), ActionError>;
    fn first_materialization_boundary(&self, path: &Path) -> Result<PathBuf, ActionError>;
    fn pin_existing_capability(
        &self,
        id: ScopeId,
        logical_path: &Path,
        capability: &PinnedDirectoryCapability,
    ) -> Result<(), ActionError>;
    /// Recoverable variant for Tonepoet-private roots recreated per run
    /// (the manual journal dir). Defaults to the plain pin for backends
    /// without a recoverability notion.
    fn pin_existing_recoverable_capability(
        &self,
        id: ScopeId,
        logical_path: &Path,
        capability: &PinnedDirectoryCapability,
    ) -> Result<(), ActionError> {
        self.pin_existing_capability(id, logical_path, capability)
    }
    fn pin_descendant_capability(
        &self,
        id: ScopeId,
        logical_path: &Path,
        ancestor_logical_path: &Path,
        capability: &PinnedDirectoryCapability,
    ) -> Result<(), ActionError>;
    fn pin_recoverable_internal_root(
        &self,
        id: ScopeId,
        path: &Path,
    ) -> Result<(), ActionError>;
    fn scoped_path(&self, path: &Path) -> Result<ScopedPath, ActionError>;
    fn scope_records(&self) -> Result<Vec<ScopeRecord>, ActionError>;
    fn validate_scope_records(&self, expected: &[ScopeRecord]) -> Result<(), ActionError>;
    fn finalize_materialized_roots(&self) -> Result<(), ActionError>;
    fn restore_scope_records(
        &self,
        records: &[ScopeRecord],
        expected_roots: &[(ScopeId, PathBuf)],
    ) -> Result<(), ActionError>;
    fn attest_materialized_scope_from_retained_direct_anchor(
        &self,
        record: &ScopeRecord,
    ) -> Result<(), ActionError>;
    fn retire_materialization_authorities_for_scope_records(
        &self,
        records: &[ScopeRecord],
    ) -> Result<(), ActionError>;
    fn retire_materialization_authorities_for_scope_records_after_terminal_marker(
        &self,
        records: &[ScopeRecord],
    ) -> Result<(), ActionError> {
        self.retire_materialization_authorities_for_scope_records(records)
    }
    fn bootstrap_read_optional(&self, path: &Path) -> Result<Option<Vec<u8>>, ActionError>;
    fn entry_identity(&self, path: &Path) -> Result<Option<CapEntryIdentity>, ActionError>;
    fn read_bytes_with_identity_optional(
        &self,
        path: &Path,
    ) -> Result<Option<(Vec<u8>, CapEntryIdentity)>, ActionError>;
    fn identity(&self, path: &Path, include_hidden: bool) -> Result<ObjectIdentity, ActionError>;
    fn identity_excluding(
        &self,
        path: &Path,
        include_hidden: bool,
        excluded_descendants: &[PathBuf],
    ) -> Result<ObjectIdentity, ActionError>;
    /// Open a reviewed regular file no-follow, verify the descriptor's complete
    /// content/object identity, and return the retained descriptor positioned
    /// at offset zero. Callers may pass this descriptor across exec boundaries;
    /// the pathname is not reopened.
    fn open_verified_regular(
        &self,
        path: &Path,
        expected: &ObjectIdentity,
    ) -> Result<File, ActionError>;
    /// Open the exact capability-scoped directory used as a script cwd. The
    /// retained descriptor is inherited by the trusted supervisor, which
    /// performs `fchdir` before spawning the exec-gated launcher.
    fn open_directory_handle(&self, path: &Path) -> Result<File, ActionError>;
    fn materialize_root_for_path(&self, path: &Path, mode: u32) -> Result<(), ActionError>;
    fn create_dir_all(&self, path: &Path) -> Result<(), ActionError>;
    fn create_private_dir_all(&self, path: &Path) -> Result<(), ActionError>;
    fn copy_to_temporary(
        &self,
        source: &Path,
        temporary: &Path,
        include_hidden: bool,
    ) -> Result<(), ActionError>;
    fn repair_copy_metadata(
        &self,
        source: &Path,
        destination: &Path,
        include_hidden: bool,
    ) -> Result<(), ActionError>;
    fn publish_no_clobber(
        &self,
        temporary: &Path,
        destination: &Path,
    ) -> Result<(), ActionError>;
    fn rename_no_clobber(
        &self,
        source: &Path,
        destination: &Path,
        expected: &ObjectIdentity,
    ) -> Result<(), ActionError>;
    fn try_move_no_clobber(
        &self,
        source: &Path,
        destination: &Path,
        expected: &ObjectIdentity,
    ) -> Result<MoveRenameAttempt, ActionError>;
    fn remove_owned_path(
        &self,
        path: &Path,
        expected: CapEntryIdentity,
    ) -> Result<(), ActionError>;
    fn sync_parent(&self, path: &Path) -> Result<(), ActionError>;
    fn path_exists_no_follow(&self, path: &Path) -> Result<bool, ActionError>;
    fn read_bytes(&self, path: &Path) -> Result<Vec<u8>, ActionError>;
    fn write_bytes_create_new_durable(
        &self,
        path: &Path,
        bytes: &[u8],
    ) -> Result<(), ActionError>;
    fn replace_owned_regular(
        &self,
        source: &Path,
        destination: &Path,
        expected_source: CapEntryIdentity,
        expected_destination: Option<CapEntryIdentity>,
    ) -> Result<(), ActionError>;
    fn directory_is_empty(&self, path: &Path) -> Result<bool, ActionError>;
    fn enumerate_tree(&self, root: &Path) -> Result<Vec<(PathBuf, ActionEntryType)>, ActionError>;
    fn enumerate_tree_cancellable(
        &self,
        root: &Path,
        cancellation: &dyn ActionCancellation,
        progress: &dyn ExplicitPreviewProgressObserver,
    ) -> Result<Vec<(PathBuf, ActionEntryType)>, ActionError>;
}

#[derive(Debug, Default)]
pub struct CapabilityActionFilesystem {
    capabilities: CapabilityFilesystem,
}

impl CapabilityActionFilesystem {
    pub fn new() -> Self {
        Self::default()
    }

    fn scoped(&self, path: &Path) -> Result<ScopedPath, ActionError> {
        self.capabilities.scoped_path(path).map_err(Into::into)
    }
}

impl ActionFilesystem for CapabilityActionFilesystem {
    fn pin_root(&self, id: ScopeId, path: &Path) -> Result<(), ActionError> {
        self.capabilities.pin_root(id, path)?;
        Ok(())
    }

    fn pin_materializable_root(&self, id: ScopeId, path: &Path) -> Result<(), ActionError> {
        self.capabilities.pin_materializable_root(id, path)?;
        Ok(())
    }

    fn first_materialization_boundary(&self, path: &Path) -> Result<PathBuf, ActionError> {
        CapabilityFilesystem::first_materialization_boundary(path).map_err(Into::into)
    }

    fn pin_existing_capability(
        &self,
        id: ScopeId,
        logical_path: &Path,
        capability: &PinnedDirectoryCapability,
    ) -> Result<(), ActionError> {
        self.capabilities
            .pin_existing_capability(id, logical_path, capability)?;
        Ok(())
    }

    fn pin_existing_recoverable_capability(
        &self,
        id: ScopeId,
        logical_path: &Path,
        capability: &PinnedDirectoryCapability,
    ) -> Result<(), ActionError> {
        self.capabilities
            .pin_existing_recoverable_capability(id, logical_path, capability)?;
        Ok(())
    }

    fn pin_descendant_capability(
        &self,
        id: ScopeId,
        logical_path: &Path,
        ancestor_logical_path: &Path,
        capability: &PinnedDirectoryCapability,
    ) -> Result<(), ActionError> {
        self.capabilities.pin_descendant_capability(
            id,
            logical_path,
            ancestor_logical_path,
            capability,
        )?;
        Ok(())
    }

    fn pin_recoverable_internal_root(
        &self,
        id: ScopeId,
        path: &Path,
    ) -> Result<(), ActionError> {
        self.capabilities.pin_recoverable_internal_root(id, path)?;
        Ok(())
    }

    fn scoped_path(&self, path: &Path) -> Result<ScopedPath, ActionError> {
        self.scoped(path)
    }

    fn scope_records(&self) -> Result<Vec<ScopeRecord>, ActionError> {
        self.capabilities.scope_records().map_err(Into::into)
    }

    fn validate_scope_records(&self, expected: &[ScopeRecord]) -> Result<(), ActionError> {
        self.capabilities.validate_scope_records(expected).map_err(Into::into)
    }

    fn finalize_materialized_roots(&self) -> Result<(), ActionError> {
        self.capabilities
            .finalize_materialized_roots()
            .map_err(Into::into)
    }

    fn restore_scope_records(
        &self,
        records: &[ScopeRecord],
        expected_roots: &[(ScopeId, PathBuf)],
    ) -> Result<(), ActionError> {
        self.capabilities
            .restore_scope_records(records, expected_roots)
            .map_err(Into::into)
    }

    fn attest_materialized_scope_from_retained_direct_anchor(
        &self,
        record: &ScopeRecord,
    ) -> Result<(), ActionError> {
        self.capabilities
            .attest_materialized_scope_from_retained_direct_anchor(record)
            .map_err(Into::into)
    }

    fn retire_materialization_authorities_for_scope_records(
        &self,
        records: &[ScopeRecord],
    ) -> Result<(), ActionError> {
        self.capabilities
            .retire_materialization_authorities_for_scope_records(records)
            .map_err(Into::into)
    }

    fn retire_materialization_authorities_for_scope_records_after_terminal_marker(
        &self,
        records: &[ScopeRecord],
    ) -> Result<(), ActionError> {
        self.capabilities
            .retire_materialization_authorities_for_scope_records_after_terminal_marker(records)
            .map_err(Into::into)
    }

    fn bootstrap_read_optional(&self, path: &Path) -> Result<Option<Vec<u8>>, ActionError> {
        CapabilityFilesystem::bootstrap_read_absolute(path).map_err(Into::into)
    }

    fn entry_identity(&self, path: &Path) -> Result<Option<CapEntryIdentity>, ActionError> {
        self.capabilities
            .entry_identity(&self.scoped(path)?)
            .map_err(Into::into)
    }

    fn read_bytes_with_identity_optional(
        &self,
        path: &Path,
    ) -> Result<Option<(Vec<u8>, CapEntryIdentity)>, ActionError> {
        self.capabilities
            .read_bytes_with_identity_optional(&self.scoped(path)?)
            .map_err(Into::into)
    }

    fn identity(&self, path: &Path, include_hidden: bool) -> Result<ObjectIdentity, ActionError> {
        capability_object_identity(&self.capabilities, &self.scoped(path)?, include_hidden, &[])
    }

    fn identity_excluding(
        &self,
        path: &Path,
        include_hidden: bool,
        excluded_descendants: &[PathBuf],
    ) -> Result<ObjectIdentity, ActionError> {
        let scoped = self.scoped(path)?;
        let mut excluded = Vec::with_capacity(excluded_descendants.len());
        for descendant in excluded_descendants {
            let candidate = self.scoped(descendant)?;
            if candidate.scope != scoped.scope
                || candidate.relative == scoped.relative
                || !candidate.relative.as_path().starts_with(scoped.relative.as_path())
            {
                return Err(ActionError::UnsafePath(format!(
                    "identity exclusion is outside its capability object: {}",
                    descendant.display()
                )));
            }
            excluded.push(candidate);
        }
        capability_object_identity(&self.capabilities, &scoped, include_hidden, &excluded)
    }

    fn open_verified_regular(
        &self,
        path: &Path,
        expected: &ObjectIdentity,
    ) -> Result<File, ActionError> {
        if expected.kind != ObjectKind::File {
            return Err(ActionError::InvalidJournal(format!(
                "retained executable identity is not a regular file: {}",
                path.display()
            )));
        }
        let scoped = self.scoped(path)?;
        let before = self
            .capabilities
            .metadata_no_follow(&scoped)?
            .ok_or_else(|| ActionError::PreviewStale(format!(
                "reviewed executable disappeared: {}",
                path.display()
            )))?;
        if before.file_type != CapFileType::Regular {
            return Err(ActionError::UnsafePath(format!(
                "reviewed executable is not a regular file: {}",
                path.display()
            )));
        }
        let mut file = self.capabilities.open_regular_read_checked(&scoped, before)?;
        let mut hasher = Sha256::new();
        let mut byte_length = 0_u64;
        let mut buffer = [0_u8; 128 * 1024];
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hasher.update(&buffer[..count]);
            byte_length = byte_length.saturating_add(count as u64);
        }
        let after = metadata_for_open_file(&file)?;
        if !script_verification_metadata_matches(before, after) {
            return Err(ActionError::PreviewStale(format!(
                "reviewed executable changed while it was being retained: {}",
                path.display()
            )));
        }
        let actual = ObjectIdentity {
            kind: ObjectKind::File,
            content_sha256: hex::encode(hasher.finalize()),
            byte_length,
            entry_count: 1,
            copy_metadata: CopyMetadataIdentity {
                root: copy_metadata_entry(PathBuf::new(), ObjectKind::File, after),
                descendants: Vec::new(),
            },
            filesystem: filesystem_identity_from_cap(after),
        };
        if !actual.same_object(expected) {
            return Err(ActionError::PreviewStale(format!(
                "reviewed executable changed before descriptor acquisition: {}",
                path.display()
            )));
        }
        if actual.copy_metadata.root.mode & 0o111 == 0 {
            return Err(ActionError::Script(format!(
                "reviewed executable is no longer executable: {}",
                path.display()
            )));
        }
        file.seek(SeekFrom::Start(0))?;
        Ok(file)
    }

    fn open_directory_handle(&self, path: &Path) -> Result<File, ActionError> {
        let scoped = self.scoped(path)?;
        Ok(File::from(self.capabilities.open_directory(&scoped)?))
    }

    fn materialize_root_for_path(&self, path: &Path, mode: u32) -> Result<(), ActionError> {
        let scoped = self.scoped(path)?;
        self.capabilities.materialize_scope(&scoped.scope, mode)?;
        Ok(())
    }

    fn create_dir_all(&self, path: &Path) -> Result<(), ActionError> {
        self.capabilities.mkdir_all(&self.scoped(path)?, 0o755)?;
        Ok(())
    }

    fn create_private_dir_all(&self, path: &Path) -> Result<(), ActionError> {
        self.capabilities.mkdir_all(&self.scoped(path)?, 0o700)?;
        Ok(())
    }

    fn copy_to_temporary(
        &self,
        source: &Path,
        temporary: &Path,
        include_hidden: bool,
    ) -> Result<(), ActionError> {
        let source = self.scoped(source)?;
        let temporary = self.scoped(temporary)?;
        if self.capabilities.metadata_no_follow(&temporary)?.is_some() {
            return Err(ActionError::Contradiction(format!(
                "journal-owned copy temporary unexpectedly exists before copy: {}",
                self.capabilities.display_path(&temporary)?.display()
            )));
        }
        self.capabilities.copy_object(&source, &temporary, include_hidden)?;
        Ok(())
    }

    fn repair_copy_metadata(
        &self,
        source: &Path,
        destination: &Path,
        include_hidden: bool,
    ) -> Result<(), ActionError> {
        self.capabilities.repair_copy_metadata(
            &self.scoped(source)?,
            &self.scoped(destination)?,
            include_hidden,
        )?;
        Ok(())
    }

    fn publish_no_clobber(
        &self,
        temporary: &Path,
        destination: &Path,
    ) -> Result<(), ActionError> {
        self.capabilities
            .publish_no_clobber(&self.scoped(temporary)?, &self.scoped(destination)?)?;
        Ok(())
    }

    fn rename_no_clobber(
        &self,
        source: &Path,
        destination: &Path,
        expected: &ObjectIdentity,
    ) -> Result<(), ActionError> {
        self.capabilities.rename_no_clobber_checked(
            &self.scoped(source)?,
            &self.scoped(destination)?,
            Some(cap_entry_identity(expected)?),
        )?;
        Ok(())
    }

    fn try_move_no_clobber(
        &self,
        source: &Path,
        destination: &Path,
        expected: &ObjectIdentity,
    ) -> Result<MoveRenameAttempt, ActionError> {
        match self.capabilities.try_rename_no_clobber_checked(
            &self.scoped(source)?,
            &self.scoped(destination)?,
            Some(cap_entry_identity(expected)?),
        )? {
            RenameNoClobberOutcome::Renamed => Ok(MoveRenameAttempt::Renamed),
            RenameNoClobberOutcome::CrossDevice => Ok(MoveRenameAttempt::CrossDevice),
        }
    }

    fn remove_owned_path(
        &self,
        path: &Path,
        expected: CapEntryIdentity,
    ) -> Result<(), ActionError> {
        self.capabilities
            .remove_tree_matching(&self.scoped(path)?, expected)?;
        Ok(())
    }

    fn sync_parent(&self, path: &Path) -> Result<(), ActionError> {
        self.capabilities.sync_parent(&self.scoped(path)?)?;
        Ok(())
    }

    fn path_exists_no_follow(&self, path: &Path) -> Result<bool, ActionError> {
        match self.capabilities.metadata_no_follow(&self.scoped(path)?)? {
            None => Ok(false),
            Some(metadata) if metadata.file_type == CapFileType::Symlink => Err(
                ActionError::UnsafePath(format!("path is a symlink: {}", path.display())),
            ),
            Some(metadata) if metadata.file_type == CapFileType::Other => Err(
                ActionError::UnsafePath(format!(
                    "path is an unsupported special file: {}",
                    path.display()
                )),
            ),
            Some(_) => Ok(true),
        }
    }

    fn read_bytes(&self, path: &Path) -> Result<Vec<u8>, ActionError> {
        self.capabilities.read_bytes(&self.scoped(path)?).map_err(Into::into)
    }

    fn write_bytes_create_new_durable(
        &self,
        path: &Path,
        bytes: &[u8],
    ) -> Result<(), ActionError> {
        self.capabilities
            .write_bytes_exclusive_durable(&self.scoped(path)?, bytes, 0o600)?;
        Ok(())
    }

    fn replace_owned_regular(
        &self,
        source: &Path,
        destination: &Path,
        expected_source: CapEntryIdentity,
        expected_destination: Option<CapEntryIdentity>,
    ) -> Result<(), ActionError> {
        self.capabilities.replace_owned_regular(
            &self.scoped(source)?,
            &self.scoped(destination)?,
            expected_source,
            expected_destination,
        )?;
        Ok(())
    }

    fn directory_is_empty(&self, path: &Path) -> Result<bool, ActionError> {
        let scoped = self.scoped(path)?;
        let expected = self
            .capabilities
            .metadata_no_follow(&scoped)?
            .ok_or_else(|| ActionError::Io(io::Error::new(
                io::ErrorKind::NotFound,
                format!("directory vanished before emptiness check: {}", path.display()),
            )))?;
        if expected.file_type != CapFileType::Directory {
            return Err(ActionError::UnsafePath(format!(
                "emptiness check target is not a directory: {}",
                path.display()
            )));
        }
        Ok(self
            .capabilities
            .enumerate_checked(&scoped, expected)?
            .is_empty())
    }

    fn enumerate_tree(&self, root: &Path) -> Result<Vec<(PathBuf, ActionEntryType)>, ActionError> {
        let scoped_root = self.scoped(root)?;
        let root_metadata = self
            .capabilities
            .metadata_no_follow(&scoped_root)?
            .ok_or_else(|| ActionError::Io(io::Error::new(
                io::ErrorKind::NotFound,
                format!("enumeration root vanished: {}", root.display()),
            )))?;
        if root_metadata.file_type != CapFileType::Directory {
            return Err(ActionError::UnsafePath(format!(
                "enumeration root is not a directory: {}",
                root.display()
            )));
        }
        let mut output = Vec::new();
        let mut stack = vec![(scoped_root, root_metadata)];
        while let Some((directory, expected_directory)) = stack.pop() {
            let mut entries = self
                .capabilities
                .enumerate_checked(&directory, expected_directory)?;
            entries.sort_by(|left, right| left.name.cmp(&right.name));
            for entry in entries {
                let child = ScopedPath {
                    scope: directory.scope.clone(),
                    relative: directory.relative.join(&entry.name)?,
                };
                let display = self.capabilities.display_path(&child)?;
                let entry_type = match entry.metadata.file_type {
                    CapFileType::Regular => ActionEntryType::File,
                    CapFileType::Directory => ActionEntryType::Directory,
                    CapFileType::Symlink => ActionEntryType::Symlink,
                    CapFileType::Other => ActionEntryType::Other,
                };
                output.push((display, entry_type));
                if entry_type == ActionEntryType::Directory {
                    stack.push((child, entry.metadata));
                }
            }
        }
        Ok(output)
    }

    fn enumerate_tree_cancellable(
        &self,
        root: &Path,
        cancellation: &dyn ActionCancellation,
        progress: &dyn ExplicitPreviewProgressObserver,
    ) -> Result<Vec<(PathBuf, ActionEntryType)>, ActionError> {
        let scoped_root = self.scoped(root)?;
        let root_metadata = self
            .capabilities
            .metadata_no_follow(&scoped_root)?
            .ok_or_else(|| ActionError::Io(io::Error::new(
                io::ErrorKind::NotFound,
                format!("enumeration root vanished: {}", root.display()),
            )))?;
        if root_metadata.file_type != CapFileType::Directory {
            return Err(ActionError::UnsafePath(format!(
                "enumeration root is not a directory: {}",
                root.display()
            )));
        }
        let mut output = Vec::new();
        let mut stack = vec![(scoped_root, root_metadata)];
        while let Some((directory, expected_directory)) = stack.pop() {
            if cancellation.is_cancelled() {
                return Err(ActionError::CancelledBeforeMutation(
                    "manual action preview preparation was cancelled".to_string(),
                ));
            }
            let mut entries = self
                .capabilities
                .enumerate_checked(&directory, expected_directory)?;
            entries.sort_by(|left, right| left.name.cmp(&right.name));
            for entry in entries {
                if cancellation.is_cancelled() {
                    return Err(ActionError::CancelledBeforeMutation(
                        "manual action preview preparation was cancelled".to_string(),
                    ));
                }
                let child = ScopedPath {
                    scope: directory.scope.clone(),
                    relative: directory.relative.join(&entry.name)?,
                };
                let display = self.capabilities.display_path(&child)?;
                let entry_type = match entry.metadata.file_type {
                    CapFileType::Regular => ActionEntryType::File,
                    CapFileType::Directory => ActionEntryType::Directory,
                    CapFileType::Symlink => ActionEntryType::Symlink,
                    CapFileType::Other => ActionEntryType::Other,
                };
                output.push((display, entry_type));
                progress.update("Scanning album entry metadata", output.len() as u64, None);
                if entry_type == ActionEntryType::Directory {
                    stack.push((child, entry.metadata));
                }
            }
        }
        Ok(output)
    }
}




// ---------------------------------------------------------------------------
// Script execution seam
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ScriptInvocation {
    pub script: PathBuf,
    pub expected_script: ObjectIdentity,
    /// Stable no-follow descriptor for the exact reviewed executable. The
    /// supervisor and launcher inherit this descriptor; they never reopen the
    /// script pathname.
    pub retained_script: Option<Arc<File>>,
    /// Retained descriptor for the exact reviewed script working directory.
    /// This is runtime-only authority and is never serialized into a journal.
    pub retained_working_directory: Option<Arc<File>>,
    pub args: Vec<String>,
    pub working_directory: PathBuf,
    pub environment: BTreeMap<String, String>,
    pub timeout: Duration,
    pub runtime_directory: PathBuf,
    pub runtime_identity: Option<RuntimeDirectoryIdentity>,
    pub containment_token: String,
}

#[derive(Debug, Clone)]
pub struct ScriptOutcome {
    pub status: ExitStatus,
    pub stdout_tail: Vec<u8>,
    pub stderr_tail: Vec<u8>,
    pub timed_out: bool,
    pub cancelled: bool,
    /// True only after the supervisor released the direct-exec invocation.
    pub started: bool,
    pub descriptor: ContainmentDescriptor,
    pub containment_empty: bool,
    pub background_descendants: bool,
    pub output_capture: OutputCaptureSummary,
}

pub trait ActionScriptRunner: Send + Sync {
    fn run(
        &self,
        invocation: &ScriptInvocation,
        cancellation: &dyn ActionCancellation,
        observer: &mut dyn FnMut(&ScriptLifecycleEvent) -> Result<(), ActionError>,
    ) -> Result<ScriptOutcome, ActionError>;

    fn recover(
        &self,
        request: &ScriptRecoveryRequest,
        observer: &mut dyn FnMut(&ScriptLifecycleEvent) -> Result<(), ActionError>,
    ) -> Result<ScriptRecoveryOutcome, ActionError>;

    fn cleanup(&self, request: &ScriptRecoveryRequest) -> Result<(), ActionError>;
}

/// Compatibility name retained for existing call sites. The implementation
/// uses the dedicated process-tree supervisor and never executes a shell.
#[derive(Debug, Default)]
pub struct ProcessGroupScriptRunner;

impl ActionScriptRunner for ProcessGroupScriptRunner {
    fn run(
        &self,
        invocation: &ScriptInvocation,
        cancellation: &dyn ActionCancellation,
        observer: &mut dyn FnMut(&ScriptLifecycleEvent) -> Result<(), ActionError>,
    ) -> Result<ScriptOutcome, ActionError> {
        if invocation.retained_script.is_none() {
            return Err(ActionError::InvalidJournal(
                "script supervisor requires a retained reviewed executable descriptor"
                    .to_string(),
            ));
        }
        if invocation.retained_working_directory.is_none() {
            return Err(ActionError::InvalidJournal(
                "script supervisor requires a retained working-directory descriptor"
                    .to_string(),
            ));
        }
        let execution_item = crate::concurrency::current_execution_item();
        let item_supervisor = execution_item
            .as_deref()
            .map(crate::concurrency::runtime_item_supervisor)
            .transpose()
            .map_err(ActionError::InvalidJournal)?;
        let command = SupervisedCommand {
                token: invocation.containment_token.clone(),
                runtime_directory: invocation.runtime_directory.clone(),
                script: invocation.script.clone(),
                script_file: invocation.retained_script.clone().ok_or_else(|| {
                    ActionError::InvalidJournal(
                        "script invocation reached the supervisor without a retained reviewed descriptor"
                            .to_string(),
                    )
                })?,
                working_directory_file: invocation
                    .retained_working_directory
                    .clone()
                    .ok_or_else(|| {
                        ActionError::InvalidJournal(
                            "script invocation reached the supervisor without a retained working-directory descriptor"
                                .to_string(),
                        )
                    })?,
                args: invocation.args.clone(),
                working_directory: invocation.working_directory.clone(),
                environment: invocation.environment.clone(),
                timeout: invocation.timeout,
                runtime_identity: invocation.runtime_identity.ok_or_else(|| {
                    ActionError::InvalidJournal(
                        "script runtime identity was not bound before supervisor launch".to_string(),
                    )
                })?,
                containment_preference: ContainmentPreference::Auto,
                helper_executable: None,
                retained_lifetime_files: if item_supervisor.is_some() {
                    Vec::new()
                } else {
                    crate::concurrency::current_supervision_lifetime_files()
                        .map_err(ActionError::InvalidJournal)?
                },
                stdin_file: None,
                stdout_file: None,
                stderr_file: None,
            };
        let containment_token = command.token.clone();
        let containment_runtime = command.runtime_directory.clone();
        let mut lifecycle = |event: &ScriptLifecycleEvent| {
            if let Some(item_id) = execution_item.as_deref() {
                match event {
                    ScriptLifecycleEvent::ContainmentPrepared { descriptor, .. } => {
                        crate::concurrency::record_execution_containment(
                            item_id, &containment_token, &containment_runtime, descriptor
                        ).map_err(crate::convert::script_supervisor::ScriptSupervisorError::Internal)?;
                    }
                    ScriptLifecycleEvent::UserCodeReleased { .. } => {
                        crate::concurrency::mark_execution_containment_released(item_id, &containment_token)
                            .map_err(crate::convert::script_supervisor::ScriptSupervisorError::Internal)?;
                    }
                    _ => {}
                }
            }
            observer(event).map_err(|error| {
                crate::convert::script_supervisor::ScriptSupervisorError::Internal(
                    format!("durable script lifecycle update failed: {error}"),
                )
            })
        };
        let outcome = match item_supervisor.as_ref() {
            Some(supervisor) => run_supervised_via_item_supervisor(
                &command, supervisor, || cancellation.is_cancelled(), &mut lifecycle
            ),
            None => run_supervised(&command, || cancellation.is_cancelled(), &mut lifecycle),
        }
        .map_err(|error| ActionError::Script(error.to_string()))?;
        if outcome.containment_empty {
            if let Some(item_id) = execution_item.as_deref() {
                let _ = crate::concurrency::clear_execution_containment(item_id, &containment_token);
            }
        }
        if let Some(warning) = outcome.descriptor.warning.as_deref() {
            log::warn!(
                "conversion action script containment used {}: {}",
                outcome.descriptor.backend.as_str(),
                warning
            );
        } else {
            log::debug!(
                "conversion action script containment backend: {}",
                outcome.descriptor.backend.as_str()
            );
        }
        Ok(ScriptOutcome {
            status: outcome.status,
            stdout_tail: outcome.stdout_tail,
            stderr_tail: outcome.stderr_tail,
            timed_out: outcome.timed_out,
            cancelled: outcome.cancelled,
            started: outcome.script_released,
            descriptor: outcome.descriptor,
            containment_empty: outcome.containment_empty,
            background_descendants: outcome.background_descendants,
            output_capture: outcome.output_capture,
        })
    }

    fn recover(
        &self,
        request: &ScriptRecoveryRequest,
        observer: &mut dyn FnMut(&ScriptLifecycleEvent) -> Result<(), ActionError>,
    ) -> Result<ScriptRecoveryOutcome, ActionError> {
        recover_supervised_with_observer(request, |event| {
            observer(event).map_err(|error| {
                crate::convert::script_supervisor::ScriptSupervisorError::Internal(
                    format!("durable script recovery lifecycle update failed: {error}"),
                )
            })
        })
        .map_err(|error| {
            ActionError::ManualRecoveryRequired(format!(
                "script containment recovery could not be completed safely: {error}"
            ))
        })
    }

    fn cleanup(&self, request: &ScriptRecoveryRequest) -> Result<(), ActionError> {
        cleanup_supervised(request).map_err(|error| {
            ActionError::Interrupted(format!(
                "script containment cleanup remains incomplete: {error}"
            ))
        })
    }
}

// ---------------------------------------------------------------------------
// Planner
// ---------------------------------------------------------------------------

pub struct ActionEngine<'a> {
    pub filesystem: &'a dyn ActionFilesystem,
    pub scripts: &'a dyn ActionScriptRunner,
}


#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManualInvocationState {
    PreviewPrepared,
    FreshExecutionAuthorized,
    RecoveryRequired,
    Executing,
    Terminal,
    CleanupComplete,
    PreviewStale,
    ManualRecoveryRequired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExplicitRecoveryOperationPreview {
    pub action_index: usize,
    pub action_kind: String,
    pub operation_id: String,
    pub summary: String,
    pub durable_state: String,
    pub cleanup_only: bool,
    pub script_started: bool,
    pub script_replayable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedExplicitInvocation {
    pub invocation_id: String,
    pub state: ManualInvocationState,
    pub authority_sha256: String,
    pub plans_serialized: String,
    pub plans: Vec<ActionPlan>,
    pub recovery_operations: Vec<ExplicitRecoveryOperationPreview>,
    pub is_recovery: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PreviewEntryKind {
    Regular,
    Directory,
    Symlink,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct PreviewEntryIdentity {
    kind: PreviewEntryKind,
    device: u64,
    inode: u64,
}

impl PreviewEntryIdentity {
    fn from_cap(identity: CapEntryIdentity) -> Self {
        let kind = match identity.file_type {
            CapFileType::Regular => PreviewEntryKind::Regular,
            CapFileType::Directory => PreviewEntryKind::Directory,
            CapFileType::Symlink => PreviewEntryKind::Symlink,
            CapFileType::Other => PreviewEntryKind::Other,
        };
        Self {
            kind,
            device: identity.device,
            inode: identity.inode,
        }
    }

    fn matches_cap(self, identity: CapEntryIdentity) -> bool {
        self == Self::from_cap(identity)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PreviewPathRoles {
    path: PathBuf,
    roles: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PreviewObjectExpectation {
    entry_identity: PreviewEntryIdentity,
    paths: Vec<PreviewPathRoles>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expected_content: Option<ObjectIdentity>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PreviewAbsentExpectation {
    path: PathBuf,
    roles: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
struct PreviewOperandGraph {
    #[serde(default)]
    objects: Vec<PreviewObjectExpectation>,
    #[serde(default)]
    absent_paths: Vec<PreviewAbsentExpectation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PreviewTreeEntry {
    path: PathBuf,
    entry_identity: PreviewEntryIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ExplicitPreviewPayload {
    schema_version: u32,
    generation: u64,
    invocation_id: String,
    state: ManualInvocationState,
    album_dir: PathBuf,
    album_identity: String,
    identity_sha256: String,
    phase: ActionPhase,
    pipeline_serialized: String,
    pipeline_sha256: String,
    claim_id: String,
    plans_serialized: String,
    plans_sha256: String,
    capability_roots: Vec<ScopeRecord>,
    subject_entry_identity: PreviewEntryIdentity,
    matcher_tree: Vec<PreviewTreeEntry>,
    operand_graph: PreviewOperandGraph,
    created_unix_nanos: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stale_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ExplicitPreviewRecord {
    payload: ExplicitPreviewPayload,
    payload_sha256: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExplicitActionRecoveryDisposition {
    Fresh,
    Resume,
    /// A terminal journal was found, but it is retired before a new command
    /// invocation is allocated. It is never reused as the result of a later
    /// `:actions-run` command.
    Terminal,
    /// The selected journal generation is terminal, but its durable
    /// write-temporary must be reconciled before retirement.
    TerminalCleanupPending,
}

struct ExplicitRecoveryReconciliation;

impl ActionCancellation for ExplicitRecoveryReconciliation {
    fn is_cancelled(&self) -> bool {
        false
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ExplicitActiveRunPayload {
    schema_version: u32,
    album_dir: PathBuf,
    album_identity: String,
    phase: ActionPhase,
    pipeline_serialized: String,
    pipeline_sha256: String,
    run_identity: String,
    journal_path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    preview_authority_sha256: Option<String>,
    created_unix_nanos: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ExplicitActiveRunRecord {
    payload: ExplicitActiveRunPayload,
    payload_sha256: String,
}

impl<'a> ActionEngine<'a> {
    pub fn plan_action(
        &self,
        action_index: usize,
        action: &ConversionAction,
        context: &ActionContext,
        claim_id: &str,
    ) -> Result<ActionPlan, ActionError> {
        prepare_and_validate_context_capabilities(self.filesystem, context)?;
        validate_phase_action(action, context)?;
        match action {
            ConversionAction::Rename(action) => {
                plan_rename(action_index, action, context, claim_id, self.filesystem)
            }
            ConversionAction::Copy(action) => {
                plan_copy(action_index, action, context, claim_id, self.filesystem)
            }
            ConversionAction::Move(action) => {
                plan_move(action_index, action, context, claim_id, self.filesystem)
            }
            ConversionAction::Delete(action) => {
                plan_delete(action_index, action, context, claim_id, self.filesystem)
            }
            ConversionAction::CreateFolder(action) => {
                plan_create_folder(action_index, action, context, self.filesystem)
            },
            ConversionAction::Runscript(action) => {
                plan_script(action_index, action, context, claim_id, self.filesystem)
            }
        }
    }

    /// Preview is planning-only. In particular, `runscript` produces a
    /// `RunScript` operation and is never handed to the runner.
    pub fn preview_action(
        &self,
        action_index: usize,
        action: &ConversionAction,
        context: &ActionContext,
    ) -> Result<ActionPlan, ActionError> {
        self.plan_action(action_index, action, context, "preview")
    }

    pub fn preview_phase(
        &self,
        pipeline: &ActionPipeline,
        context: &ActionContext,
    ) -> Result<Vec<ActionPlan>, ActionError> {
        pipeline
            .for_phase(context.phase)
            .iter()
            .enumerate()
            .map(|(index, action)| self.preview_action(index, action, context))
            .collect()
    }

    /// Prepare the exact manual action invocation that the user will review.
    /// Fresh invocations persist the complete serialized plans before the
    /// album authority is released. Recovery invocations are rendered only
    /// from the durable journal (or the linked prepared-plan authority when a
    /// crash occurred before the journal's first generation).
    #[allow(dead_code)] // bundle-provided API surface, not yet wired to a caller
    pub(crate) fn prepare_explicit_invocation_with_lock(
        &self,
        pipeline: &ActionPipeline,
        context: &ActionContext,
        identity_sha256: &str,
        lock: &ExplicitActionRunLock,
    ) -> Result<PreparedExplicitInvocation, ActionError> {
        self.prepare_explicit_invocation_with_lock_cancellable(
            pipeline,
            context,
            identity_sha256,
            &NeverCancel,
            lock,
        )
    }

    #[allow(dead_code)] // bundle-provided API surface, not yet wired to a caller
    pub(crate) fn prepare_explicit_invocation_with_lock_cancellable(
        &self,
        pipeline: &ActionPipeline,
        context: &ActionContext,
        identity_sha256: &str,
        cancellation: &dyn ActionCancellation,
        lock: &ExplicitActionRunLock,
    ) -> Result<PreparedExplicitInvocation, ActionError> {
        self.prepare_explicit_invocation_with_lock_cancellable_observed(
            pipeline,
            context,
            identity_sha256,
            cancellation,
            &NoExplicitPreviewProgress,
            lock,
        )
    }

    pub(crate) fn prepare_explicit_invocation_with_lock_cancellable_observed(
        &self,
        pipeline: &ActionPipeline,
        context: &ActionContext,
        identity_sha256: &str,
        cancellation: &dyn ActionCancellation,
        progress: &dyn ExplicitPreviewProgressObserver,
        lock: &ExplicitActionRunLock,
    ) -> Result<PreparedExplicitInvocation, ActionError> {
        progress.update("Inspecting durable recovery authority", 0, None);
        if !context.explicit_scope || context.phase != ActionPhase::Post {
            return Err(ActionError::Conflict(
                "prepared explicit invocation requires a post-phase explicit context".to_string(),
            ));
        }
        let bound_context = bind_explicit_context_to_lock(context, lock)?;
        let context = &bound_context;
        lock.validate_context(context)?;
        if cancellation.is_cancelled() {
            return Err(ActionError::CancelledBeforeMutation(
                "manual action preview preparation was cancelled".to_string(),
            ));
        }
        loop {
            if cancellation.is_cancelled() {
                return Err(ActionError::CancelledBeforeMutation(
                    "manual action preview preparation was cancelled".to_string(),
                ));
            }
            if let Some((active, recorded_pipeline, active_context)) =
                load_explicit_active_run_for_context_locked(context, lock)?
            {
                match self.scan_explicit_recovery_for_context_locked(
                    &recorded_pipeline,
                    &active_context,
                    true,
                )? {
                    ExplicitActionRecoveryDisposition::Terminal => {
                        self.retire_terminal_active_and_preview_locked(
                            &active,
                            &active_context,
                            lock,
                        )?;
                        continue;
                    }
                    ExplicitActionRecoveryDisposition::TerminalCleanupPending => {
                        self.execute_phase(
                            &recorded_pipeline,
                            &active_context,
                            &ExplicitRecoveryReconciliation,
                        )?;
                        self.retire_terminal_active_and_preview_locked(
                            &active,
                            &active_context,
                            lock,
                        )?;
                        continue;
                    }
                    ExplicitActionRecoveryDisposition::Fresh
                    | ExplicitActionRecoveryDisposition::Resume => {
                        validate_explicit_active_run_pipeline_matches(&active, pipeline)?;
                        return self.prepared_recovery_invocation_locked(
                            &active,
                            &recorded_pipeline,
                            &active_context,
                            identity_sha256,
                            lock,
                        );
                    }
                }
            }

            // The preview authority is the stable discovery record for a
            // crash between user review, active-pointer publication, and the
            // first journal generation. Inspect it before scanning legacy
            // deterministic journals so its UUID-backed journal can never
            // become an unknown orphan.
            if let Some((orphan_preview, _)) = load_explicit_preview_locked(context, lock)? {
                let preview_pipeline: ActionPipeline = serde_json::from_str(
                    &orphan_preview.payload.pipeline_serialized,
                )
                .map_err(ActionError::Serialization)?;
                if preview_pipeline.canonical_serialization()?
                    != pipeline.canonical_serialization()?
                {
                    return Err(ActionError::ManualRecoveryRequired(format!(
                        "manual invocation {} was prepared with a different action pipeline",
                        orphan_preview.payload.invocation_id
                    )));
                }
                if orphan_preview.payload.album_identity != context.album_identity
                    || orphan_preview.payload.identity_sha256 != identity_sha256
                {
                    return Err(ActionError::ManualRecoveryRequired(format!(
                        "manual invocation {} no longer matches canonical album identity",
                        orphan_preview.payload.invocation_id
                    )));
                }
                let mut orphan_context = context.clone();
                orphan_context.run_identity = orphan_preview.payload.invocation_id.clone();
                let orphan_digest = preview_pipeline.canonical_sha256()?;
                let orphan_journal_path = action_journal_path(&orphan_context, &orphan_digest)?;
                let orphan_store = JournalStore::new(orphan_journal_path, self.filesystem)?;
                let orphan_journal_exists = load_journal_bootstrap(&orphan_store)?.is_some();

                match orphan_preview.payload.state {
                    ManualInvocationState::PreviewPrepared
                    | ManualInvocationState::PreviewStale => {
                        if orphan_journal_exists {
                            return Err(ActionError::Contradiction(
                                "an unexecuted preview unexpectedly owns a durable journal"
                                    .to_string(),
                            ));
                        }
                        remove_explicit_preview_locked(&orphan_preview, context, lock)?;
                    }
                    ManualInvocationState::FreshExecutionAuthorized => {
                        let active = create_explicit_active_run_locked(
                            &preview_pipeline,
                            &orphan_context,
                            orphan_context.run_identity.clone(),
                            Some(explicit_preview_binding_sha256(&orphan_preview.payload)?),
                            lock,
                        )?;
                        return self.prepared_recovery_invocation_locked(
                            &active,
                            &preview_pipeline,
                            &orphan_context,
                            identity_sha256,
                            lock,
                        );
                    }
                    ManualInvocationState::RecoveryRequired
                    | ManualInvocationState::Executing => {
                        if !orphan_journal_exists {
                            return Err(ActionError::ManualRecoveryRequired(format!(
                                "manual invocation {} lost its active pointer and durable journal",
                                orphan_preview.payload.invocation_id
                            )));
                        }
                        let active = create_explicit_active_run_locked(
                            &preview_pipeline,
                            &orphan_context,
                            orphan_context.run_identity.clone(),
                            Some(explicit_preview_binding_sha256(&orphan_preview.payload)?),
                            lock,
                        )?;
                        return self.prepared_recovery_invocation_locked(
                            &active,
                            &preview_pipeline,
                            &orphan_context,
                            identity_sha256,
                            lock,
                        );
                    }
                    ManualInvocationState::Terminal
                    | ManualInvocationState::CleanupComplete => {
                        if orphan_journal_exists {
                            if !explicit_journal_is_resolved_terminal(
                                self.filesystem,
                                &preview_pipeline,
                                &orphan_context,
                            )? {
                                let active = create_explicit_active_run_locked(
                                    &preview_pipeline,
                                    &orphan_context,
                                    orphan_context.run_identity.clone(),
                                    Some(explicit_preview_binding_sha256(
                                        &orphan_preview.payload,
                                    )?),
                                    lock,
                                )?;
                                return self.prepared_recovery_invocation_locked(
                                    &active,
                                    &preview_pipeline,
                                    &orphan_context,
                                    identity_sha256,
                                    lock,
                                );
                            }
                            retire_resolved_terminal_journal_locked(
                                self.filesystem,
                                &preview_pipeline,
                                &orphan_context,
                            )?;
                        }
                        remove_explicit_preview_locked(&orphan_preview, context, lock)?;
                    }
                    ManualInvocationState::ManualRecoveryRequired => {
                        return Err(ActionError::ManualRecoveryRequired(format!(
                            "manual invocation {} requires administrative recovery",
                            orphan_preview.payload.invocation_id
                        )));
                    }
                }
            }

            // Adopt an unresolved journal left by the pre-preview-authority
            // implementation. Terminal generations are retired, never reused.
            match self.scan_explicit_recovery_for_context_locked(pipeline, context, false)? {
                ExplicitActionRecoveryDisposition::Resume => {
                    let active = create_explicit_active_run_locked(
                        pipeline,
                        context,
                        context.run_identity.clone(),
                        None,
                        lock,
                    )?;
                    return self.prepared_recovery_invocation_locked(
                        &active,
                        pipeline,
                        context,
                        identity_sha256,
                        lock,
                    );
                }
                ExplicitActionRecoveryDisposition::Terminal => {
                    retire_resolved_terminal_journal_locked(self.filesystem, pipeline, context)?;
                    continue;
                }
                ExplicitActionRecoveryDisposition::TerminalCleanupPending => {
                    self.execute_phase(pipeline, context, &ExplicitRecoveryReconciliation)?;
                    retire_resolved_terminal_journal_locked(self.filesystem, pipeline, context)?;
                    continue;
                }
                ExplicitActionRecoveryDisposition::Fresh => {}
            }

            let invocation_id = format!("manual-invocation:{}", Uuid::new_v4());
            let claim_id = Uuid::new_v4().to_string();
            let mut fresh_context = context.clone();
            fresh_context.run_identity = invocation_id.clone();

            // The preview itself is stored beneath this directory, so establish
            // it before retaining capability identities. This avoids treating
            // our own authority publication as an external root replacement.
            let _authority = lock.manual_authority_capability(true)?;
            prepare_context_capabilities(self.filesystem, &fresh_context)?;
            prepare_pipeline_capabilities(self.filesystem, pipeline, &fresh_context)?;
            let configured_actions = pipeline.for_phase(fresh_context.phase);
            progress.update(
                "Planning configured actions",
                0,
                Some(configured_actions.len() as u64),
            );
            let mut plans = Vec::new();
            for (index, action) in configured_actions.iter().enumerate() {
                if cancellation.is_cancelled() {
                    return Err(ActionError::CancelledBeforeMutation(
                        "manual action preview preparation was cancelled".to_string(),
                    ));
                }
                plans.push(self.plan_action(index, action, &fresh_context, &claim_id)?);
                progress.update(
                    "Planning configured actions",
                    (index + 1) as u64,
                    Some(configured_actions.len() as u64),
                );
            }
            let plans_serialized =
                serde_json::to_string(&plans).map_err(ActionError::Serialization)?;
            let pipeline_serialized = pipeline.canonical_serialization()?;
            progress.update("Scanning album entry metadata", 0, None);
            let (subject_entry_identity, matcher_tree) = capture_preview_matcher_tree_cancellable(
                self.filesystem,
                &fresh_context,
                cancellation,
                progress,
            )?;
            progress.update(
                "Scanning album entry metadata",
                matcher_tree.len() as u64,
                Some(matcher_tree.len() as u64),
            );
            let binding_total = plans
                .iter()
                .map(|plan| {
                    plan.operations.len() as u64
                        + plan.planning_preconditions.len() as u64
                })
                .sum::<u64>();
            progress.update("Binding reviewed plan facts", 0, Some(binding_total));
            let operand_graph = preview_operand_graph_cancellable_observed(
                self.filesystem,
                &plans,
                cancellation,
                progress,
                binding_total,
            )?;
            if cancellation.is_cancelled() {
                return Err(ActionError::CancelledBeforeMutation(
                    "manual action preview preparation was cancelled".to_string(),
                ));
            }
            let payload = ExplicitPreviewPayload {
                schema_version: EXPLICIT_PREVIEW_SCHEMA_VERSION,
                generation: 0,
                invocation_id: invocation_id.clone(),
                state: ManualInvocationState::PreviewPrepared,
                album_dir: fresh_context.album_dir.clone(),
                album_identity: fresh_context.album_identity.clone(),
                identity_sha256: identity_sha256.to_string(),
                phase: fresh_context.phase,
                pipeline_sha256: sha256_hex(pipeline_serialized.as_bytes()),
                pipeline_serialized,
                claim_id,
                plans_sha256: sha256_hex(plans_serialized.as_bytes()),
                plans_serialized: plans_serialized.clone(),
                capability_roots: self.filesystem.scope_records()?,
                subject_entry_identity,
                matcher_tree,
                operand_graph,
                created_unix_nanos: now_unix_nanos(),
                stale_reason: None,
            };
            progress.update("Committing reviewed preview authority", 0, Some(1));
            let record = explicit_preview_record(payload)?;
            write_explicit_preview_locked(&record, None, &fresh_context, lock)?;
            progress.update("Committing reviewed preview authority", 1, Some(1));
            return Ok(PreparedExplicitInvocation {
                invocation_id,
                state: ManualInvocationState::PreviewPrepared,
                authority_sha256: explicit_preview_binding_sha256(&record.payload)?,
                plans_serialized,
                plans,
                recovery_operations: Vec::new(),
                is_recovery: false,
            });
        }
    }

    fn prepared_recovery_invocation_locked(
        &self,
        active: &ExplicitActiveRunRecord,
        pipeline: &ActionPipeline,
        context: &ActionContext,
        identity_sha256: &str,
        lock: &ExplicitActionRunLock,
    ) -> Result<PreparedExplicitInvocation, ActionError> {
        lock.validate_context(context)?;
        let store = JournalStore::new(active.payload.journal_path.clone(), self.filesystem)?;
        let journal = load_journal_bootstrap(&store)?;
        if let Some((journal, _)) = journal {
            self.filesystem.restore_scope_records(
                &journal.capability_roots,
                &expected_capability_roots(pipeline, context, &journal.capability_roots)?,
            )?;
            prepare_context_capabilities(self.filesystem, context)?;
            prepare_pipeline_capabilities(self.filesystem, pipeline, context)?;
            validate_journal(
                &journal,
                self.filesystem,
                context,
                pipeline,
                &active.payload.pipeline_serialized,
                &active.payload.pipeline_sha256,
            )?;
            let plans = journal
                .actions
                .iter()
                .map(|action| action.plan.clone())
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| ActionError::ManualRecoveryRequired(
                    "the durable manual journal does not yet contain every original plan"
                        .to_string(),
                ))?;
            let plans_serialized =
                serde_json::to_string(&plans).map_err(ActionError::Serialization)?;
            let existing = load_explicit_preview_locked(context, lock)?;
            let record = if let Some((record, _)) = existing.as_ref() {
                let binding = explicit_preview_binding_sha256(&record.payload)?;
                if active.payload.preview_authority_sha256.as_deref()
                    != Some(binding.as_str())
                    || record.payload.invocation_id != active.payload.run_identity
                    || record.payload.plans_serialized != plans_serialized
                {
                    return Err(ActionError::Contradiction(
                        "manual recovery journal disagrees with its preview authority".to_string(),
                    ));
                }
                if record.payload.state == ManualInvocationState::RecoveryRequired {
                    record.clone()
                } else {
                    self.transition_explicit_preview_locked(
                        record.clone(),
                        ManualInvocationState::RecoveryRequired,
                        None,
                        context,
                        lock,
                    )?
                }
            } else {
                if active.payload.preview_authority_sha256.is_some() {
                    return Err(ActionError::ManualRecoveryRequired(
                        "manual recovery preview authority is missing".to_string(),
                    ));
                }
                let payload = ExplicitPreviewPayload {
                    schema_version: EXPLICIT_PREVIEW_SCHEMA_VERSION,
                    generation: 0,
                    invocation_id: active.payload.run_identity.clone(),
                    state: ManualInvocationState::RecoveryRequired,
                    album_dir: context.album_dir.clone(),
                    album_identity: context.album_identity.clone(),
                    identity_sha256: identity_sha256.to_string(),
                    phase: context.phase,
                    pipeline_serialized: active.payload.pipeline_serialized.clone(),
                    pipeline_sha256: active.payload.pipeline_sha256.clone(),
                    claim_id: journal.claim_id.clone(),
                    plans_sha256: sha256_hex(plans_serialized.as_bytes()),
                    plans_serialized: plans_serialized.clone(),
                    capability_roots: journal.capability_roots.clone(),
                    subject_entry_identity: preview_entry_identity(
                        self.filesystem,
                        &context.subject_dir,
                        "recovery subject",
                    )?,
                    matcher_tree: Vec::new(),
                    operand_graph: PreviewOperandGraph::default(),
                    created_unix_nanos: now_unix_nanos(),
                    stale_reason: None,
                };
                let record = explicit_preview_record(payload)?;
                write_explicit_preview_locked(&record, None, context, lock)?;
                record
            };
            return Ok(PreparedExplicitInvocation {
                invocation_id: active.payload.run_identity.clone(),
                state: ManualInvocationState::RecoveryRequired,
                authority_sha256: explicit_preview_binding_sha256(&record.payload)?,
                plans_serialized,
                plans,
                recovery_operations: recovery_operation_previews(&journal),
                is_recovery: true,
            });
        }

        let preview_checksum = active
            .payload
            .preview_authority_sha256
            .as_deref()
            .ok_or_else(|| ActionError::ManualRecoveryRequired(
                "active manual invocation has neither a journal nor linked preview authority"
                    .to_string(),
            ))?;
        let (record, _) = load_explicit_preview_locked(context, lock)?
            .ok_or_else(|| ActionError::ManualRecoveryRequired(
                "active manual invocation lost its linked preview authority".to_string(),
            ))?;
        if explicit_preview_binding_sha256(&record.payload)? != preview_checksum
            || record.payload.invocation_id != active.payload.run_identity
        {
            return Err(ActionError::Contradiction(
                "active manual invocation points to a different preview authority".to_string(),
            ));
        }
        let record = if record.payload.state == ManualInvocationState::RecoveryRequired {
            record
        } else {
            self.transition_explicit_preview_locked(
                record,
                ManualInvocationState::RecoveryRequired,
                None,
                context,
                lock,
            )?
        };
        let plans: Vec<ActionPlan> = serde_json::from_str(&record.payload.plans_serialized)
            .map_err(ActionError::Serialization)?;
        Ok(PreparedExplicitInvocation {
            invocation_id: record.payload.invocation_id.clone(),
            state: ManualInvocationState::RecoveryRequired,
            authority_sha256: explicit_preview_binding_sha256(&record.payload)?,
            plans_serialized: record.payload.plans_serialized.clone(),
            plans,
            recovery_operations: Vec::new(),
            is_recovery: true,
        })
    }

    pub(crate) fn discard_prepared_explicit_preview_with_lock(
        &self,
        context: &ActionContext,
        invocation_id: &str,
        authority_sha256: &str,
        lock: &ExplicitActionRunLock,
    ) -> Result<(), ActionError> {
        let bound_context = bind_explicit_context_to_lock(context, lock)?;
        let context = &bound_context;
        lock.validate_context(context)?;
        if load_explicit_active_run_for_context_locked(context, lock)?.is_some() {
            return Err(ActionError::Conflict(
                "cannot discard a preview after execution authority was created".to_string(),
            ));
        }
        let (preview, _) = load_explicit_preview_locked(context, lock)?
            .ok_or_else(|| ActionError::PreviewStale(
                "prepared preview authority is already absent".to_string(),
            ))?;
        if preview.payload.invocation_id != invocation_id
            || explicit_preview_binding_sha256(&preview.payload)? != authority_sha256
        {
            return Err(ActionError::PreviewStale(
                "prepared preview authority was replaced".to_string(),
            ));
        }
        if !matches!(
            preview.payload.state,
            ManualInvocationState::PreviewPrepared | ManualInvocationState::PreviewStale
        ) {
            return Err(ActionError::Conflict(format!(
                "cannot discard manual invocation in state {:?}",
                preview.payload.state
            )));
        }
        remove_explicit_preview_locked(&preview, context, lock)
    }

    /// Execute only the concrete serialized plans represented by the reviewed
    /// preview authority. This method never invokes the planner.
    pub(crate) fn execute_prepared_explicit_phase_with_lock(
        &self,
        pipeline: &ActionPipeline,
        context: &ActionContext,
        identity_sha256: &str,
        invocation_id: &str,
        authority_sha256: &str,
        cancellation: &dyn ActionCancellation,
        lock: &mut ExplicitActionRunLock,
    ) -> Result<ActionPhaseReport, ActionError> {
        let bound_context = bind_explicit_context_to_lock(context, lock)?;
        let context = &bound_context;
        lock.validate_context(context)?;
        let (mut preview, _) = load_explicit_preview_locked(context, lock)?
            .ok_or_else(|| ActionError::PreviewStale(
                "reviewed preview authority is missing".to_string(),
            ))?;
        if explicit_preview_binding_sha256(&preview.payload)? != authority_sha256
            || preview.payload.invocation_id != invocation_id
        {
            return Err(ActionError::PreviewStale(
                "reviewed preview authority was replaced".to_string(),
            ));
        }
        let pipeline_serialized = pipeline.canonical_serialization()?;
        if preview.payload.pipeline_serialized != pipeline_serialized
            || preview.payload.pipeline_sha256 != sha256_hex(pipeline_serialized.as_bytes())
            || preview.payload.album_identity != context.album_identity
            || preview.payload.album_dir != context.album_dir
            || preview.payload.phase != context.phase
        {
            return self.mark_preview_stale_and_fail(
                preview,
                context,
                lock,
                "canonical action pipeline or album context changed",
            );
        }
        let plans: Vec<ActionPlan> = serde_json::from_str(&preview.payload.plans_serialized)
            .map_err(ActionError::Serialization)?;

        if let Some((active, recorded_pipeline, active_context)) =
            load_explicit_active_run_for_context_locked(context, lock)?
        {
            validate_explicit_active_run_pipeline_matches(&active, pipeline)?;
            if active.payload.run_identity != invocation_id
                || active.payload.preview_authority_sha256.as_deref()
                    != Some(authority_sha256)
            {
                return Err(ActionError::ManualRecoveryRequired(
                    "a different unresolved manual invocation owns this album".to_string(),
                ));
            }
            preview = self.transition_explicit_preview_locked(
                preview,
                ManualInvocationState::Executing,
                None,
                &active_context,
                lock,
            )?;
            // The durable active-run plus separate action-execution lock now
            // own recovery and exclusion. Release the short publication
            // transition lock before any filesystem action or script runs.
            lock.release_publication_authority();
            let result = self.execute_phase_with_prepared_plans(
                &recorded_pipeline,
                &active_context,
                cancellation,
                &preview.payload.claim_id,
                &plans,
            );
            return self.finish_prepared_explicit_execution(
                result,
                &active,
                &recorded_pipeline,
                &active_context,
                preview,
                lock,
            );
        }

        if preview.payload.state != ManualInvocationState::PreviewPrepared {
            return Err(ActionError::PreviewStale(format!(
                "reviewed invocation is in state {:?}",
                preview.payload.state
            )));
        }
        if let Err(error) =
            self.validate_fresh_preview_snapshot(
                &preview,
                context,
                identity_sha256,
                pipeline,
                &plans,
            )
        {
            return match error {
                other @ (ActionError::InvalidJournal(_) | ActionError::Serialization(_)) => {
                    Err(other)
                }
                ActionError::PreviewStale(reason) => {
                    self.mark_preview_stale_and_fail(preview, context, lock, &reason)
                }
                other => self.mark_preview_stale_and_fail(
                    preview,
                    context,
                    lock,
                    &other.to_string(),
                ),
            };
        }
        if let Err(error) = self.validate_prepared_plans_against_pipeline(
            pipeline,
            context,
            &preview.payload.claim_id,
            &plans,
        ) {
            return match error {
                other @ (ActionError::InvalidJournal(_) | ActionError::UnsafePath(_)) => {
                    Err(other)
                }
                other => self.mark_preview_stale_and_fail(
                    preview,
                    context,
                    lock,
                    &other.to_string(),
                ),
            };
        }
        if cancellation.is_cancelled() {
            let cleanup = self.transition_explicit_preview_locked(
                preview,
                ManualInvocationState::CleanupComplete,
                None,
                context,
                lock,
            )?;
            remove_explicit_preview_locked(&cleanup, context, lock)?;
            return Err(ActionError::CancelledBeforeMutation(
                "reviewed manual invocation was cancelled before execution authorization"
                    .to_string(),
            ));
        }
        preview = self.transition_explicit_preview_locked(
            preview,
            ManualInvocationState::FreshExecutionAuthorized,
            None,
            context,
            lock,
        )?;
        let mut execution_context = context.clone();
        execution_context.run_identity = invocation_id.to_string();
        let active = create_explicit_active_run_locked(
            pipeline,
            &execution_context,
            invocation_id.to_string(),
            Some(explicit_preview_binding_sha256(&preview.payload)?),
            lock,
        )?;
        preview = self.transition_explicit_preview_locked(
            preview,
            ManualInvocationState::Executing,
            None,
            &execution_context,
            lock,
        )?;
        // The reviewed invocation is now durably authorized and protected by
        // the action-execution lock. Do not retain the publication lock across
        // arbitrary actions or scripts.
        lock.release_publication_authority();
        let result = self.execute_phase_with_prepared_plans(
            pipeline,
            &execution_context,
            cancellation,
            &preview.payload.claim_id,
            &plans,
        );
        self.finish_prepared_explicit_execution(
            result,
            &active,
            pipeline,
            &execution_context,
            preview,
            lock,
        )
    }

    fn validate_fresh_preview_snapshot(
        &self,
        preview: &ExplicitPreviewRecord,
        context: &ActionContext,
        identity_sha256: &str,
        pipeline: &ActionPipeline,
        plans: &[ActionPlan],
    ) -> Result<(), ActionError> {
        if preview.payload.identity_sha256 != identity_sha256 {
            return Err(ActionError::PreviewStale(
                "canonical album identity changed".to_string(),
            ));
        }
        self.filesystem.restore_scope_records(
            &preview.payload.capability_roots,
            &expected_capability_roots(
                pipeline,
                context,
                &preview.payload.capability_roots,
            )?,
        )?;
        self.filesystem
            .validate_scope_records(&preview.payload.capability_roots)?;
        let (current_subject, current_matcher_tree) =
            capture_preview_matcher_tree(self.filesystem, context)?;
        if current_subject != preview.payload.subject_entry_identity {
            return Err(ActionError::PreviewStale(
                "album root capability object changed after preview".to_string(),
            ));
        }
        if current_matcher_tree != preview.payload.matcher_tree {
            return Err(ActionError::PreviewStale(
                "album directory entries changed after preview".to_string(),
            ));
        }
        validate_preview_operand_graph(self.filesystem, &preview.payload.operand_graph)?;
        validate_planning_preconditions(self.filesystem, plans)?;
        Ok(())
    }

    fn mark_preview_stale_and_fail<T>(
        &self,
        preview: ExplicitPreviewRecord,
        context: &ActionContext,
        lock: &ExplicitActionRunLock,
        reason: &str,
    ) -> Result<T, ActionError> {
        let _ = self.transition_explicit_preview_locked(
            preview,
            ManualInvocationState::PreviewStale,
            Some(reason.to_string()),
            context,
            lock,
        )?;
        Err(ActionError::PreviewStale(reason.to_string()))
    }

    fn transition_explicit_preview_locked(
        &self,
        previous: ExplicitPreviewRecord,
        state: ManualInvocationState,
        stale_reason: Option<String>,
        context: &ActionContext,
        lock: &ExplicitActionRunLock,
    ) -> Result<ExplicitPreviewRecord, ActionError> {
        let mut payload = previous.payload.clone();
        payload.generation = payload.generation.checked_add(1).ok_or_else(|| {
            ActionError::InvalidJournal(
                "explicit preview authority generation overflow".to_string(),
            )
        })?;
        payload.state = state;
        payload.stale_reason = stale_reason;
        let next = explicit_preview_record(payload)?;
        write_explicit_preview_locked(&next, Some(&previous), context, lock)?;
        Ok(next)
    }

    fn retire_terminal_active_and_preview_locked(
        &self,
        active: &ExplicitActiveRunRecord,
        context: &ActionContext,
        lock: &ExplicitActionRunLock,
    ) -> Result<(), ActionError> {
        let preview = load_explicit_preview_locked(context, lock)?;
        let terminal_preview = if let Some((preview, _)) = preview {
            if let Some(expected) = active.payload.preview_authority_sha256.as_deref() {
                if explicit_preview_binding_sha256(&preview.payload)? != expected
                    || preview.payload.invocation_id != active.payload.run_identity
                {
                    return Err(ActionError::Contradiction(
                        "terminal manual journal disagrees with preview authority".to_string(),
                    ));
                }
            }
            Some(if preview.payload.state == ManualInvocationState::Terminal {
                preview
            } else {
                self.transition_explicit_preview_locked(
                    preview,
                    ManualInvocationState::Terminal,
                    None,
                    context,
                    lock,
                )?
            })
        } else {
            None
        };
        retire_completed_explicit_run_locked(self.filesystem, active, context, lock)?;
        if let Some(terminal_preview) = terminal_preview {
            let cleanup = self.transition_explicit_preview_locked(
                terminal_preview,
                ManualInvocationState::CleanupComplete,
                None,
                context,
                lock,
            )?;
            remove_explicit_preview_locked(&cleanup, context, lock)?;
        }
        Ok(())
    }

    fn finish_prepared_explicit_execution(
        &self,
        result: Result<ActionPhaseReport, ActionError>,
        active: &ExplicitActiveRunRecord,
        pipeline: &ActionPipeline,
        context: &ActionContext,
        preview: ExplicitPreviewRecord,
        lock: &ExplicitActionRunLock,
    ) -> Result<ActionPhaseReport, ActionError> {
        if explicit_journal_is_resolved_terminal(self.filesystem, pipeline, context)? {
            let terminal = self.transition_explicit_preview_locked(
                preview,
                ManualInvocationState::Terminal,
                None,
                context,
                lock,
            )?;
            retire_completed_explicit_run_locked(
                self.filesystem,
                active,
                context,
                lock,
            )?;
            let cleanup = self.transition_explicit_preview_locked(
                terminal,
                ManualInvocationState::CleanupComplete,
                None,
                context,
                lock,
            )?;
            remove_explicit_preview_locked(&cleanup, context, lock)?;
        }
        result
    }


    fn attest_pre_album_scope_from_retained_publication(
        &self,
        context: &ActionContext,
        records: &[ScopeRecord],
    ) -> Result<(), ActionError> {
        if context.phase != ActionPhase::Pre || context.retained_album_capability.is_none() {
            return Ok(());
        }
        for record in records.iter().filter(|record| record.id.as_str() == "album") {
            self.filesystem
                .attest_materialized_scope_from_retained_direct_anchor(record)?;
        }
        Ok(())
    }

    /// Load the PRE journal (if any) through the retained publication
    /// capabilities and publish the album's materialized identity attestation.
    /// The publish path calls this while the publication authority is bound,
    /// so the attestation exists before any participant finalizes and
    /// validates the journal without live album capabilities (e.g. a failed
    /// item's no-binding finalize arriving ahead of every with-binding one).
    pub fn attest_pre_album_scope_for_publication(
        &self,
        pipeline: &ActionPipeline,
        context: &ActionContext,
    ) -> Result<(), ActionError> {
        validate_context_syntax(context)?;
        if context.phase != ActionPhase::Pre
            || pipeline.for_phase(context.phase).is_empty()
        {
            return Ok(());
        }
        let pipeline_serialized = pipeline.canonical_serialization()?;
        let pipeline_sha256 = sha256_hex(pipeline_serialized.as_bytes());
        let journal_path = action_journal_path(context, &pipeline_sha256)?;
        let store = JournalStore::new(journal_path, self.filesystem)?;
        let retained_live_context = prepare_context_for_journal_read(self.filesystem, context)?;
        if !retained_live_context {
            return Ok(());
        }
        let Some((journal, _from_write_temporary)) = load_journal_bound(&store)? else {
            return Ok(());
        };
        self.attest_pre_album_scope_from_retained_publication(context, &journal.capability_roots)
    }

    /// Read the durable report associated with this exact pipeline/context.
    /// This is primarily used by orchestration to surface a complete report
    /// when execution returns a terminal error. A foreign, changed, malformed,
    /// or contradictory journal still fails closed through `validate_journal`.
    pub fn durable_phase_report(
        &self,
        pipeline: &ActionPipeline,
        context: &ActionContext,
    ) -> Result<Option<ActionPhaseReport>, ActionError> {
        validate_context_syntax(context)?;
        if pipeline.for_phase(context.phase).is_empty() {
            if context_has_retained_capabilities(context) {
                prepare_and_validate_context_capabilities(self.filesystem, context)?;
            } else {
                validate_context(context)?;
            }
            return Ok(Some(ActionPhaseReport {
                phase: Some(context.phase),
                ..ActionPhaseReport::default()
            }));
        }
        let pipeline_serialized = pipeline.canonical_serialization()?;
        let pipeline_sha256 = sha256_hex(pipeline_serialized.as_bytes());
        let journal_path = action_journal_path(context, &pipeline_sha256)?;
        #[cfg(test)]
        let test_journal_path = journal_path.clone();
        let store = JournalStore::new(journal_path, self.filesystem)?;
        let retained_live_context = prepare_context_for_journal_read(self.filesystem, context)?;
        let loaded = if retained_live_context {
            load_journal_bound(&store)?
        } else {
            load_journal_bootstrap(&store)?
        };
        let Some((journal, _from_write_temporary)) = loaded else {
            return Ok(None);
        };
        #[cfg(test)]
        test_pause_durable_report_after_journal_load(&test_journal_path)?;
        if retained_live_context {
            self.attest_pre_album_scope_from_retained_publication(
                context,
                &journal.capability_roots,
            )?;
            prepare_retained_pipeline_capabilities(self.filesystem, pipeline, context)?;
            self.filesystem.restore_scope_records(
                &journal.capability_roots,
                &expected_capability_roots(pipeline, context, &journal.capability_roots)?,
            )?;
            prepare_pipeline_capabilities(self.filesystem, pipeline, context)?;
            self.filesystem.validate_scope_records(&journal.capability_roots)?;
        } else {
            self.filesystem.restore_scope_records(
                &journal.capability_roots,
                &expected_capability_roots(pipeline, context, &journal.capability_roots)?,
            )?;
            prepare_context_capabilities(self.filesystem, context)?;
            validate_context_through_capabilities(self.filesystem, context)?;
            prepare_pipeline_capabilities(self.filesystem, pipeline, context)?;
        }
        validate_journal(
            &journal,
            self.filesystem,
            context,
            pipeline,
            &pipeline_serialized,
            &pipeline_sha256,
        )?;
        Ok(Some(report_from_journal(&journal)?))
    }

    /// Retire PRE materialization authorities only after orchestration has
    /// reached the terminal batch cleanup gate. The first pass reloads the
    /// durable journal through retained publication descriptors and removes
    /// authority files only after authenticating the journal token, scope/base
    /// identity, and retained published object identity. Marked retries load the
    /// journal without live album capabilities and remove only remaining
    /// scope/token/base-authenticated authority files because the durable marker
    /// proves that object-identity validation already completed.
    pub fn retire_terminal_materialization_authorities(
        &self,
        pipeline: &ActionPipeline,
        context: &ActionContext,
        before_authority_unlink: Option<&dyn Fn() -> Result<(), ActionError>>,
        authority_retirement_previously_marked: bool,
    ) -> Result<(), ActionError> {
        validate_context_syntax(context)?;
        if pipeline.for_phase(context.phase).is_empty() {
            return Ok(());
        }
        let pipeline_serialized = pipeline.canonical_serialization()?;
        let pipeline_sha256 = sha256_hex(pipeline_serialized.as_bytes());
        let journal_path = action_journal_path(context, &pipeline_sha256)?;
        let store = JournalStore::new(journal_path, self.filesystem)?;

        if authority_retirement_previously_marked {
            // The orchestration layer durably records this state only after a
            // previous pass validated the terminal PRE journal against the
            // retained published object. If that process then died before
            // workspace cleanup, retry must not depend on reopening the album
            // path: the user may have deleted the album after authority
            // retirement began. Load the durable journal without preparing live
            // scope capabilities and retry only descriptor-relative removal of
            // any remaining scope/token/base-authenticated authority files.
            let Some((journal, _from_write_temporary)) = load_journal_bootstrap(&store)? else {
                return Ok(());
            };
            self.filesystem
                .retire_materialization_authorities_for_scope_records_after_terminal_marker(
                    &journal.capability_roots,
                )?;
            return Ok(());
        }

        let retained_live_context = prepare_context_for_journal_read(self.filesystem, context)?;
        let loaded = if retained_live_context {
            load_journal_bound(&store)?
        } else {
            load_journal_bootstrap(&store)?
        };
        let Some((journal, _from_write_temporary)) = loaded else {
            return Ok(());
        };
        if !retained_live_context {
            return Ok(());
        }

        self.attest_pre_album_scope_from_retained_publication(
            context,
            &journal.capability_roots,
        )?;
        prepare_retained_pipeline_capabilities(self.filesystem, pipeline, context)?;
        self.filesystem.restore_scope_records(
            &journal.capability_roots,
            &expected_capability_roots(pipeline, context, &journal.capability_roots)?,
        )?;
        prepare_pipeline_capabilities(self.filesystem, pipeline, context)?;
        self.filesystem.validate_scope_records(&journal.capability_roots)?;
        validate_journal(
            &journal,
            self.filesystem,
            context,
            pipeline,
            &pipeline_serialized,
            &pipeline_sha256,
        )?;
        if let Some(before_authority_unlink) = before_authority_unlink {
            before_authority_unlink()?;
        }
        self.filesystem
            .retire_materialization_authorities_for_scope_records(&journal.capability_roots)?;
        Ok(())
    }

    /// Validate all durable authority beneath an explicit/manual action root.
    /// A stable active-run record discovers interrupted work, while each truly
    /// new command receives a fresh invocation identity and journal pathname.
    pub fn inspect_explicit_recovery(
        &self,
        pipeline: &ActionPipeline,
        context: &ActionContext,
    ) -> Result<ExplicitActionRecoveryDisposition, ActionError> {
        if !context.explicit_scope {
            return Err(ActionError::Conflict(
                "explicit recovery inspection requires an explicit-scope action context".to_string(),
            ));
        }
        let lock = acquire_explicit_action_run_lock(context)?;
        self.inspect_explicit_recovery_with_lock(pipeline, context, &lock)
    }

    /// Inspect explicit recovery while the caller holds the album's shared
    /// identity/manual-run lock. This is used by preview so identity loading,
    /// recovery inspection, and planning observe one serialized snapshot.
    pub(crate) fn inspect_explicit_recovery_with_lock(
        &self,
        pipeline: &ActionPipeline,
        context: &ActionContext,
        lock: &ExplicitActionRunLock,
    ) -> Result<ExplicitActionRecoveryDisposition, ActionError> {
        let bound_context = bind_explicit_context_to_lock(context, lock)?;
        let context = &bound_context;
        lock.validate_context(context)?;
        if let Some((record, recorded_pipeline, active_context)) =
            load_explicit_active_run_for_context_locked(context, lock)?
        {
            let disposition = self.scan_explicit_recovery_for_context_locked(
                &recorded_pipeline,
                &active_context,
                true,
            )?;
            if matches!(
                disposition,
                ExplicitActionRecoveryDisposition::Terminal
                    | ExplicitActionRecoveryDisposition::TerminalCleanupPending
            ) {
                return Ok(disposition);
            }
            validate_explicit_active_run_pipeline_matches(&record, pipeline)?;
            return Ok(disposition);
        }
        self.scan_explicit_recovery_for_context_locked(pipeline, context, false)
    }

    /// Execute an explicit/manual phase under a stable per-album process lock.
    /// Unresolved work resumes through `.active-run.json`; a completed journal
    /// is retired before a fresh UUID-backed command invocation is allocated.
    #[cfg(test)]
    pub fn execute_explicit_phase(
        &self,
        pipeline: &ActionPipeline,
        context: &ActionContext,
        cancellation: &dyn ActionCancellation,
    ) -> Result<ActionPhaseReport, ActionError> {
        if !context.explicit_scope {
            return Err(ActionError::Conflict(
                "explicit phase execution requires an explicit-scope action context".to_string(),
            ));
        }
        let mut lock = acquire_explicit_action_run_lock(context)?;
        self.execute_explicit_phase_with_lock(pipeline, context, cancellation, &mut lock)
    }

    /// Execute while the caller holds the shared identity/manual-run lock.
    /// Callers must reread and validate the canonical identity under this same
    /// lock before constructing `context`.
    #[cfg(test)]
    pub(crate) fn execute_explicit_phase_with_lock(
        &self,
        pipeline: &ActionPipeline,
        context: &ActionContext,
        cancellation: &dyn ActionCancellation,
        lock: &mut ExplicitActionRunLock,
    ) -> Result<ActionPhaseReport, ActionError> {
        let bound_context = bind_explicit_context_to_lock(context, lock)?;
        let context = &bound_context;
        lock.validate_context(context)?;
        if pipeline.for_phase(context.phase).is_empty() {
            return self.execute_phase(pipeline, context, cancellation);
        }
        let (active_record, execution_context) =
            self.prepare_explicit_execution_context_locked(pipeline, context, lock)?;
        lock.release_publication_authority();
        let result = self.execute_phase(pipeline, &execution_context, cancellation);

        // Terminal success, deterministic failure, and cancellation-before-
        // mutation all publish a validated cleanup-complete terminal journal.
        // Retire that authority after cloning the report/error. Recoverable or
        // indeterminate outcomes intentionally retain the active pointer.
        if explicit_journal_is_resolved_terminal(
            self.filesystem,
            pipeline,
            &execution_context,
        )? {
            retire_completed_explicit_run_locked(
                self.filesystem,
                &active_record,
                &execution_context,
                lock,
            )?;
        }
        result
    }

    #[cfg(test)]
    fn prepare_explicit_execution_context_locked(
        &self,
        pipeline: &ActionPipeline,
        context: &ActionContext,
        lock: &ExplicitActionRunLock,
    ) -> Result<(ExplicitActiveRunRecord, ActionContext), ActionError> {
        loop {
            if let Some((record, recorded_pipeline, active_context)) =
                load_explicit_active_run_for_context_locked(context, lock)?
            {
                match self.scan_explicit_recovery_for_context_locked(
                    &recorded_pipeline,
                    &active_context,
                    true,
                )? {
                    ExplicitActionRecoveryDisposition::Terminal => {
                        retire_completed_explicit_run_locked(
                            self.filesystem,
                            &record,
                            &active_context,
                            lock,
                        )?;
                        continue;
                    }
                    ExplicitActionRecoveryDisposition::TerminalCleanupPending => {
                        self.execute_phase(
                            &recorded_pipeline,
                            &active_context,
                            &ExplicitRecoveryReconciliation,
                        )?;
                        retire_completed_explicit_run_locked(
                            self.filesystem,
                            &record,
                            &active_context,
                            lock,
                        )?;
                        continue;
                    }
                    ExplicitActionRecoveryDisposition::Fresh
                    | ExplicitActionRecoveryDisposition::Resume => {
                        validate_explicit_active_run_pipeline_matches(&record, pipeline)?;
                        return Ok((record, active_context));
                    }
                }
            }

            // Compatibility migration for pre-active-pointer builds: an exact
            // deterministic unresolved journal may be adopted once. Terminal
            // deterministic journals are retired and never replayed.
            match self.scan_explicit_recovery_for_context_locked(pipeline, context, false)? {
                ExplicitActionRecoveryDisposition::Resume => {
                    let record = create_explicit_active_run_locked(
                        pipeline,
                        context,
                        context.run_identity.clone(),
                        None,
                        lock,
                    )?;
                    return Ok((record, context.clone()));
                }
                ExplicitActionRecoveryDisposition::Terminal => {
                    retire_resolved_terminal_journal_locked(
                        self.filesystem,
                        pipeline,
                        context,
                    )?;
                }
                ExplicitActionRecoveryDisposition::TerminalCleanupPending => {
                    self.execute_phase(
                        pipeline,
                        context,
                        &ExplicitRecoveryReconciliation,
                    )?;
                    retire_resolved_terminal_journal_locked(
                        self.filesystem,
                        pipeline,
                        context,
                    )?;
                }
                ExplicitActionRecoveryDisposition::Fresh => {}
            }

            let run_identity = format!("manual-invocation:{}", Uuid::new_v4());
            let mut fresh_context = context.clone();
            fresh_context.run_identity = run_identity.clone();
            let record = create_explicit_active_run_locked(
                pipeline,
                &fresh_context,
                run_identity,
                None,
                lock,
            )?;
            return Ok((record, fresh_context));
        }
    }

    fn scan_explicit_recovery_for_context_locked(
        &self,
        pipeline: &ActionPipeline,
        context: &ActionContext,
        active_pointer_present: bool,
    ) -> Result<ExplicitActionRecoveryDisposition, ActionError> {
        let retained_live_context =
            prepare_context_for_journal_read(self.filesystem, context)?;
        let pipeline_serialized = pipeline.canonical_serialization()?;
        let pipeline_sha256 = sha256_hex(pipeline_serialized.as_bytes());
        let expected_journal_path = action_journal_path(context, &pipeline_sha256)?;
        let expected_store = JournalStore::new(expected_journal_path.clone(), self.filesystem)?;
        let expected = if retained_live_context {
            load_journal_bound(&expected_store)?
        } else {
            load_journal_bootstrap(&expected_store)?
        };

        let mut authorized_entries = BTreeSet::<PathBuf>::new();
        authorized_entries.insert(explicit_action_run_lock_path(context));
        authorized_entries.insert(explicit_active_run_path(context));
        authorized_entries.insert(explicit_active_run_temporary_path(context));
        authorized_entries.insert(explicit_preview_path(context));
        authorized_entries.insert(explicit_preview_temporary_path(context));
        authorized_entries.insert(expected_journal_path.clone());
        authorized_entries.insert(expected_store.temporary.clone());

        retire_orphan_terminal_explicit_journals_locked(
            self.filesystem,
            context,
            &authorized_entries,
        )?;

        let disposition = if let Some((journal, _loaded_from_temporary)) = expected.as_ref() {
            if retained_live_context {
                prepare_retained_pipeline_capabilities(self.filesystem, pipeline, context)?;
            }
            self.filesystem.restore_scope_records(
                &journal.capability_roots,
                &expected_capability_roots(pipeline, context, &journal.capability_roots)?,
            )?;
            if !retained_live_context {
                prepare_context_capabilities(self.filesystem, context)?;
            }
            prepare_pipeline_capabilities(self.filesystem, pipeline, context)?;
            validate_journal(
                journal,
                self.filesystem,
                context,
                pipeline,
                &pipeline_serialized,
                &pipeline_sha256,
            )?;
            for workspace in authorized_workspace_paths(journal) {
                if workspace.starts_with(&context.journal_dir) {
                    authorized_entries.insert(workspace);
                }
            }
            match journal.terminal.as_ref() {
                Some(terminal)
                    if terminal.cleanup_complete
                        && !terminal.report.recovery_required
                        && validate_resolved_terminal_journal_authority_for_context(
                            self.filesystem,
                            journal,
                            &expected_journal_path,
                            retained_live_context,
                        )
                        .is_ok() =>
                {
                    ExplicitActionRecoveryDisposition::Terminal
                }
                Some(terminal)
                    if terminal.cleanup_complete
                        && !terminal.report.recovery_required
                        && validate_terminal_journal_authority_without_write_temporary(
                            journal,
                            &expected_journal_path,
                        )
                        .is_ok() =>
                {
                    ExplicitActionRecoveryDisposition::TerminalCleanupPending
                }
                _ => ExplicitActionRecoveryDisposition::Resume,
            }
        } else if active_pointer_present {
            // A crash may occur after publishing the active pointer and before
            // the first journal write. Reuse that invocation identity.
            ExplicitActionRecoveryDisposition::Resume
        } else {
            ExplicitActionRecoveryDisposition::Fresh
        };

        validate_explicit_authority_directory_locked(
            self.filesystem,
            context,
            &expected_journal_path,
            &expected_store.temporary,
            &authorized_entries,
        )?;
        Ok(disposition)
    }
}

fn validate_explicit_authority_directory_locked(
    filesystem: &dyn ActionFilesystem,
    context: &ActionContext,
    expected_journal_path: &Path,
    expected_temporary_path: &Path,
    authorized_entries: &BTreeSet<PathBuf>,
) -> Result<(), ActionError> {
    if let Some(journal_capability) = context.retained_journal_capability.as_deref() {
        for (name, identity) in journal_capability.list_entries()? {
            let path = context.journal_dir.join(&name);
            if identity.file_type == CapFileType::Symlink {
                return Err(ActionError::Conflict(format!(
                    "manual action authority contains a symlink and fails closed: {}",
                    path.display()
                )));
            }
            if path == explicit_action_run_lock_path(context)
                || path == explicit_active_run_path(context)
                || path == explicit_active_run_temporary_path(context)
                || path == explicit_preview_path(context)
                || path == explicit_preview_temporary_path(context)
                || path == expected_journal_path
                || path == expected_temporary_path
            {
                if identity.file_type != CapFileType::Regular {
                    return Err(ActionError::Conflict(format!(
                        "manual action authority control path is not a regular file: {}",
                        path.display()
                    )));
                }
                continue;
            }
            if authorized_entries.iter().any(|authorized| {
                path == *authorized
                    || path.starts_with(authorized)
                    || authorized.starts_with(&path)
            }) {
                continue;
            }

            let name_text = name.to_string_lossy();
            if name_text.starts_with("actions-")
                && name_text.ends_with(".journal.json")
            {
                if identity.file_type != CapFileType::Regular {
                    return Err(ActionError::Conflict(format!(
                        "manual action journal is not a regular file: {}",
                        path.display()
                    )));
                }
                let (bytes, observed) = journal_capability
                    .read_regular_child_optional(&name)?
                    .ok_or_else(|| {
                        ActionError::Contradiction(format!(
                            "manual action journal vanished during fail-closed recovery scan: {}",
                            path.display()
                        ))
                    })?;
                if observed != identity {
                    return Err(ActionError::Contradiction(format!(
                        "manual action journal changed during fail-closed recovery scan: {}",
                        path.display()
                    )));
                }
                let _journal = deserialize_action_journal(&bytes)?;
                return Err(ActionError::Conflict(format!(
                    "an unresolved prior manual action journal conflicts with this invocation: {}. Resume through its exact active-run authority or complete administrative recovery",
                    path.display()
                )));
            }

            return Err(ActionError::Conflict(format!(
                "manual action authority contains orphaned or unrecognized recovery state: {}",
                path.display()
            )));
        }
        return Ok(());
    }

    let entries = match fs::read_dir(&context.journal_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            return Err(ActionError::Conflict(format!(
                "manual action authority contains a symlink and fails closed: {}",
                path.display()
            )));
        }
        if path == explicit_action_run_lock_path(context)
            || path == explicit_active_run_path(context)
            || path == explicit_active_run_temporary_path(context)
            || path == explicit_preview_path(context)
            || path == explicit_preview_temporary_path(context)
            || path == expected_journal_path
            || path == expected_temporary_path
        {
            if !file_type.is_file() {
                return Err(ActionError::Conflict(format!(
                    "manual action authority control path is not a regular file: {}",
                    path.display()
                )));
            }
            continue;
        }
        if authorized_entries.iter().any(|authorized| {
            path == *authorized
                || path.starts_with(authorized)
                || authorized.starts_with(&path)
        }) {
            continue;
        }

        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("actions-") && name.ends_with(".journal.json") {
            if !file_type.is_file() {
                return Err(ActionError::Conflict(format!(
                    "manual action journal is not a regular file: {}",
                    path.display()
                )));
            }
            let bytes = filesystem.bootstrap_read_optional(&path)?.ok_or_else(|| {
                ActionError::Contradiction(format!(
                    "manual action journal vanished during fail-closed recovery scan: {}",
                    path.display()
                ))
            })?;
            let _journal = deserialize_action_journal(&bytes)?;
            return Err(ActionError::Conflict(format!(
                "an unresolved prior manual action journal conflicts with this invocation: {}. Resume through its exact active-run authority or complete administrative recovery",
                path.display()
            )));
        }

        return Err(ActionError::Conflict(format!(
            "manual action authority contains orphaned or unrecognized recovery state: {}",
            path.display()
        )));
    }
    Ok(())
}

fn retire_orphan_terminal_explicit_journals_locked(
    filesystem: &dyn ActionFilesystem,
    context: &ActionContext,
    authorized_entries: &BTreeSet<PathBuf>,
) -> Result<(), ActionError> {
    if let Some(journal_capability) = context.retained_journal_capability.as_deref() {
        return retire_orphan_terminal_explicit_journals_bound_locked(
            filesystem,
            context,
            authorized_entries,
            journal_capability,
        );
    }
    retire_orphan_terminal_explicit_journals_lexical_locked(context, authorized_entries)
}

fn retire_orphan_terminal_explicit_journals_bound_locked(
    _filesystem: &dyn ActionFilesystem,
    context: &ActionContext,
    authorized_entries: &BTreeSet<PathBuf>,
    journal_capability: &PinnedDirectoryCapability,
) -> Result<(), ActionError> {
    let mut candidates = Vec::<OsString>::new();
    for (name, identity) in journal_capability.list_entries()? {
        let path = context.journal_dir.join(&name);
        if identity.file_type == CapFileType::Symlink {
            return Err(ActionError::Conflict(format!(
                "manual action authority contains a symlink and fails closed: {}",
                path.display()
            )));
        }
        let name_text = name.to_string_lossy();
        if identity.file_type == CapFileType::Regular
            && name_text.starts_with("actions-")
            && name_text.ends_with(".journal.json")
            && !authorized_entries.contains(&path)
        {
            candidates.push(name);
        }
    }

    for journal_name in candidates {
        let journal_path = context.journal_dir.join(&journal_name);
        let temporary_path = journal_write_temporary_path(&journal_path)?;
        let temporary_name = temporary_path.file_name().ok_or_else(|| {
            ActionError::UnsafePath(format!(
                "manual journal temporary has no file name: {}",
                temporary_path.display()
            ))
        })?;
        let Some((final_bytes, final_identity)) =
            journal_capability.read_regular_child_optional(&journal_name)?
        else {
            continue;
        };
        let temporary = journal_capability.read_regular_child_optional(temporary_name)?;
        let final_journal = deserialize_action_journal(&final_bytes)?;
        let (terminal_authority, temporary_is_newer_authority) = match temporary.as_ref() {
            None => (final_journal.clone(), false),
            Some((bytes, _identity)) => {
                let temporary_journal = deserialize_action_journal(bytes)?;
                validate_owned_journal_generation(&temporary_journal, &final_journal)?;
                if temporary_journal.generation > final_journal.generation {
                    (temporary_journal, true)
                } else if final_journal.generation > temporary_journal.generation
                    || temporary_journal == final_journal
                {
                    (final_journal.clone(), false)
                } else {
                    return Err(ActionError::Contradiction(
                        "orphaned manual journal generations disagree at the same generation"
                            .to_string(),
                    ));
                }
            }
        };
        if validate_terminal_journal_authority_without_write_temporary(
            &terminal_authority,
            &journal_path,
        )
        .is_err()
        {
            continue;
        }

        let mut final_bytes_for_removal = final_bytes;
        let mut final_identity_for_removal = final_identity;
        if let Some((temporary_bytes, temporary_identity)) = temporary.as_ref() {
            let current_temporary = journal_capability
                .read_regular_child_optional(temporary_name)?
                .ok_or_else(|| {
                    ActionError::Contradiction(format!(
                        "manual journal temporary vanished before retirement: {}",
                        temporary_path.display()
                    ))
                })?;
            let current_final = journal_capability
                .read_regular_child_optional(&journal_name)?
                .ok_or_else(|| {
                    ActionError::Contradiction(format!(
                        "manual journal vanished before retirement: {}",
                        journal_path.display()
                    ))
                })?;
            if current_temporary.0 != *temporary_bytes
                || current_temporary.1 != *temporary_identity
                || current_final.0 != final_bytes_for_removal
                || current_final.1 != final_identity_for_removal
            {
                return Err(ActionError::Contradiction(format!(
                    "manual journal changed before descriptor-relative retirement: {}",
                    journal_path.display()
                )));
            }
            if temporary_is_newer_authority {
                journal_capability.replace_regular_child(
                    temporary_name,
                    &journal_name,
                    *temporary_identity,
                    Some(final_identity_for_removal),
                )?;
                let (promoted_bytes, promoted_identity) = journal_capability
                    .read_regular_child_optional(&journal_name)?
                    .ok_or_else(|| {
                        ActionError::Contradiction(format!(
                            "promoted manual journal vanished: {}",
                            journal_path.display()
                        ))
                    })?;
                if promoted_bytes != *temporary_bytes {
                    return Err(ActionError::Contradiction(format!(
                        "promoted manual journal content changed: {}",
                        journal_path.display()
                    )));
                }
                final_bytes_for_removal = promoted_bytes;
                final_identity_for_removal = promoted_identity;
            } else {
                journal_capability.remove_regular_child_if_identity(
                    temporary_name,
                    *temporary_identity,
                )?;
            }
        }

        let current_final = journal_capability
            .read_regular_child_optional(&journal_name)?
            .ok_or_else(|| {
                ActionError::Contradiction(format!(
                    "manual journal vanished before final retirement: {}",
                    journal_path.display()
                ))
            })?;
        if current_final.0 != final_bytes_for_removal
            || current_final.1 != final_identity_for_removal
        {
            return Err(ActionError::Contradiction(format!(
                "manual journal changed before final descriptor-relative retirement: {}",
                journal_path.display()
            )));
        }
        journal_capability
            .remove_regular_child_if_identity(&journal_name, final_identity_for_removal)?;
    }
    Ok(())
}

fn retire_orphan_terminal_explicit_journals_lexical_locked(
    context: &ActionContext,
    authorized_entries: &BTreeSet<PathBuf>,
) -> Result<(), ActionError> {
    let entries = match fs::read_dir(&context.journal_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let mut candidates = Vec::new();
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            return Err(ActionError::Conflict(format!(
                "manual action authority contains a symlink and fails closed: {}",
                path.display()
            )));
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if file_type.is_file()
            && name.starts_with("actions-")
            && name.ends_with(".journal.json")
            && !authorized_entries.contains(&path)
        {
            candidates.push(path);
        }
    }

    for journal_path in candidates {
        let temporary_path = journal_write_temporary_path(&journal_path)?;
        let final_bytes = read_regular_file_optional_no_follow(&journal_path)?;
        let temporary_bytes = read_regular_file_optional_no_follow(&temporary_path)?;
        let Some(final_bytes) = final_bytes else {
            continue;
        };
        let final_journal = deserialize_action_journal(&final_bytes)?;
        let (terminal_authority, temporary_is_newer_authority) = match temporary_bytes.as_ref() {
            None => (final_journal.clone(), false),
            Some(bytes) => {
                let temporary_journal = deserialize_action_journal(bytes)?;
                validate_owned_journal_generation(&temporary_journal, &final_journal)?;
                if temporary_journal.generation > final_journal.generation {
                    (temporary_journal, true)
                } else if final_journal.generation > temporary_journal.generation
                    || temporary_journal == final_journal
                {
                    (final_journal.clone(), false)
                } else {
                    return Err(ActionError::Contradiction(
                        "orphaned manual journal generations disagree at the same generation"
                            .to_string(),
                    ));
                }
            }
        };
        if validate_terminal_journal_authority_without_write_temporary(
            &terminal_authority,
            &journal_path,
        )
        .is_err()
        {
            continue;
        }

        // Revalidate exact bytes immediately before each transition. The album
        // lock excludes Tonepoet writers; byte identity also rejects outside
        // replacement between inspection and retirement. If the write-temporary
        // is the newer terminal generation, atomically promote it over the older
        // final generation before deleting anything. A crash then leaves a
        // terminal final journal, never an older nonterminal generation that
        // could wedge or be mistaken for resumable work.
        let mut final_bytes_for_removal = final_bytes;
        if let Some(bytes) = temporary_bytes.as_ref() {
            revalidate_exact_file(&temporary_path, bytes)?;
            revalidate_exact_file(&journal_path, &final_bytes_for_removal)?;
            if temporary_is_newer_authority {
                fs::rename(&temporary_path, &journal_path)?;
                sync_parent(&journal_path)?;
                final_bytes_for_removal = bytes.clone();
            } else {
                fs::remove_file(&temporary_path)?;
                sync_parent(&temporary_path)?;
            }
        }
        revalidate_exact_file(&journal_path, &final_bytes_for_removal)?;
        fs::remove_file(&journal_path)?;
        sync_parent(&journal_path)?;
    }
    Ok(())
}


#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ExplicitInProcessAuthorityKey {
    parent_device: u64,
    parent_inode: u64,
    album_component: OsString,
}

fn explicit_in_process_lock_keys() -> &'static Mutex<BTreeSet<ExplicitInProcessAuthorityKey>> {
    static LOCKS: OnceLock<Mutex<BTreeSet<ExplicitInProcessAuthorityKey>>> = OnceLock::new();
    LOCKS.get_or_init(|| Mutex::new(BTreeSet::new()))
}

fn explicit_lock_registry_mutex() -> &'static Mutex<()> {
    static REGISTRY: OnceLock<Mutex<()>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(()))
}

fn validated_album_component(album_dir: &Path) -> Result<OsString, ActionError> {
    if is_filesystem_root(album_dir) {
        return Err(ActionError::UnsafePath(format!(
            "manual action authority refuses a filesystem root: {}",
            album_dir.display()
        )));
    }
    let component = album_dir.file_name().ok_or_else(|| {
        ActionError::UnsafePath(format!(
            "manual action album has no final component: {}",
            album_dir.display()
        ))
    })?;
    let relative = checked_relative_target(&component.to_string_lossy())?;
    if relative.components().count() != 1 {
        return Err(ActionError::UnsafePath(format!(
            "manual action album component is not stable: {}",
            album_dir.display()
        )));
    }
    Ok(component.to_os_string())
}

fn open_album_parent_capability(
    album_dir: &Path,
) -> Result<(PinnedDirectoryCapability, OsString, PathBuf), ActionError> {
    let album_dir = lexical_normalize_absolute(album_dir)?;
    let component = validated_album_component(&album_dir)?;
    let parent = album_dir.parent().ok_or_else(|| {
        ActionError::UnsafePath(format!(
            "manual action album has no parent directory: {}",
            album_dir.display()
        ))
    })?;
    // The configured route may contain symlinks. Resolve and open it once,
    // then retain the directory object. Every authority mutation below is
    // descriptor-relative and therefore cannot be redirected by later lexical
    // replacement of the configured path.
    let parent_capability = PinnedDirectoryCapability::open_trusted(parent)?;

    // When the album already exists, bind the authority name to the opened
    // directory object rather than to the caller's spelling. This collapses
    // case-folding and Unicode-normalization aliases on filesystems that expose
    // the same child through more than one lexical representation.
    let component = match parent_capability.entry_identity(&component)? {
        Some(target_identity) => parent_capability
            .list_entries()?
            .into_iter()
            .find_map(|(name, identity)| (identity == target_identity).then_some(name))
            .unwrap_or(component),
        None => component,
    };
    let canonical_album_dir = parent_capability.display_path().join(&component);
    Ok((parent_capability, component, canonical_album_dir))
}

fn open_album_parent_capability_under_output_root(
    album_dir: &Path,
    logical_output_root: &Path,
    output_root_capability: &PinnedDirectoryCapability,
) -> Result<(PinnedDirectoryCapability, OsString, PathBuf), ActionError> {
    let album_dir = lexical_normalize_absolute(album_dir)?;
    let logical_output_root = lexical_normalize_absolute(logical_output_root)?;
    let component = validated_album_component(&album_dir)?;
    let parent = album_dir.parent().ok_or_else(|| {
        ActionError::UnsafePath(format!(
            "album publication target has no parent directory: {}",
            album_dir.display()
        ))
    })?;
    let parent_relative = parent.strip_prefix(&logical_output_root).map_err(|_| {
        ActionError::UnsafePath(format!(
            "album publication parent {} is outside retained output root {}",
            parent.display(),
            logical_output_root.display()
        ))
    })?;
    let parent_capability = output_root_capability
        .open_directory_descendant(parent_relative, true, 0o755)?;
    let component = match parent_capability.entry_identity(&component)? {
        Some(target_identity) => parent_capability
            .list_entries()?
            .into_iter()
            .find_map(|(name, identity)| (identity == target_identity).then_some(name))
            .unwrap_or(component),
        None => component,
    };
    let canonical_album_dir = logical_output_root
        .join(parent_relative)
        .join(&component);
    Ok((parent_capability, component, canonical_album_dir))
}

fn release_explicit_in_process_lock_key(key: &ExplicitInProcessAuthorityKey) {
    explicit_in_process_lock_keys()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(key);
}

#[derive(Debug)]
pub(crate) struct ExplicitActionRunLock {
    publication_file: Option<File>,
    publication_lock_name: OsString,
    publication_lock_identity: CapEntryIdentity,
    action_file: Option<File>,
    action_lock_name: Option<OsString>,
    action_lock_identity: Option<CapEntryIdentity>,
    album_dir: PathBuf,
    album_component: OsString,
    #[allow(dead_code)] // bundle-provided API surface, not yet wired to a caller
    journal_dir: PathBuf,
    parent_capability: PinnedDirectoryCapability,
    /// Exact output/coordination root retained by production publication.
    /// Manual-only locks leave this unset because they do not own a pipeline
    /// output-root transaction.
    output_root_capability: Option<PinnedDirectoryCapability>,
    logical_output_root: Option<PathBuf>,
    album_identity_at_acquire: Option<CapEntryIdentity>,
    in_process_key: ExplicitInProcessAuthorityKey,
}

impl ExplicitActionRunLock {
    fn validate_context(&self, context: &ActionContext) -> Result<(), ActionError> {
        let expected_album_dir = lexical_normalize_absolute(&self.album_dir)?;
        let context_album_dir = lexical_normalize_absolute(&context.album_dir)?;
        if context_album_dir != expected_album_dir {
            return Err(ActionError::Conflict(format!(
                "manual action authority for {} cannot authorize album {}",
                self.album_dir.display(),
                context.album_dir.display()
            )));
        }
        let journal_name = context.journal_dir.file_name().ok_or_else(|| {
            ActionError::Conflict("manual action context journal root has no name".to_string())
        })?;
        if journal_name != OsStr::new(".tonepoet-actions-manual") {
            return Err(ActionError::Conflict(format!(
                "manual action authority for {} cannot authorize context journal root {}",
                self.album_dir.display(),
                context.journal_dir.display()
            )));
        }
        let journal_parent = context.journal_dir.parent().ok_or_else(|| {
            ActionError::Conflict("manual action context journal root has no parent".to_string())
        })?;
        if lexical_normalize_absolute(journal_parent)? != expected_album_dir {
            return Err(ActionError::Conflict(format!(
                "manual action authority for {} cannot authorize context journal root {}",
                self.album_dir.display(),
                context.journal_dir.display()
            )));
        }

        let retained = (
            context.retained_album_capability.as_deref(),
            context.retained_output_capability.as_deref(),
            context.retained_journal_capability.as_deref(),
        );
        match retained {
            (Some(album), Some(output), Some(journal)) => {
                let locked_album = self.album_capability()?;
                if album.identity() != locked_album.identity() {
                    return Err(ActionError::Conflict(format!(
                        "manual action context album capability does not match the locked album object {}",
                        self.album_dir.display()
                    )));
                }
                let locked_output_identity = match &self.output_root_capability {
                    Some(capability) => capability.identity(),
                    None => self.parent_capability.identity(),
                };
                if output.identity() != locked_output_identity {
                    return Err(ActionError::Conflict(format!(
                        "manual action context output capability does not match the locked output authority {}",
                        self.output_root_capability
                            .as_ref()
                            .unwrap_or(&self.parent_capability)
                            .display_path()
                            .display()
                    )));
                }
                let locked_journal = self.manual_authority_capability(false)?;
                if journal.identity() != locked_journal.identity() {
                    return Err(ActionError::Conflict(format!(
                        "manual action context journal capability does not match the locked manual authority {}",
                        context.journal_dir.display()
                    )));
                }
                Ok(())
            }
            (None, None, None) => {
                // Genuine restart/legacy recovery may reconstruct a lexical
                // context before it has retained capabilities. Preserve that
                // compatibility path, but never use it for a live explicit run.
                self.validate_album_dir(&context.album_dir)?;
                self.validate_album_dir(journal_parent)
            }
            _ => Err(ActionError::Contradiction(
                "manual action context carries only a partial retained capability set"
                    .to_string(),
            )),
        }
    }

    pub(crate) fn validate_album_dir(&self, album_dir: &Path) -> Result<(), ActionError> {
        let (parent, component, canonical_album_dir) = open_album_parent_capability(album_dir)?;
        if parent.identity() != self.parent_capability.identity()
            || component != self.album_component
            || canonical_album_dir != self.album_dir
        {
            return Err(ActionError::Conflict(format!(
                "manual action authority for {} cannot authorize album {}",
                self.album_dir.display(),
                album_dir.display()
            )));
        }
        Ok(())
    }

    pub(crate) fn album_capability(&self) -> Result<PinnedDirectoryCapability, ActionError> {
        let capability = self.parent_capability.open_directory_child(
            &self.album_component,
            false,
            0o755,
        )?;
        Ok(capability)
    }

    /// Clone the exact directory objects that define a manual action context.
    /// Stable lexical paths remain in journals and UI state, but preview,
    /// execution, and recovery bind those paths to these retained descriptors.
    pub(crate) fn retained_manual_context_capabilities(
        &self,
    ) -> Result<
        (
            PinnedDirectoryCapability,
            PinnedDirectoryCapability,
            PinnedDirectoryCapability,
        ),
        ActionError,
    > {
        let album = self.album_capability()?;
        // Prefer the exact retained output-root capability; the album's
        // immediate parent is only correct when the album sits directly in
        // the output root.
        let output = match &self.output_root_capability {
            Some(capability) => capability.try_clone()?,
            None => self.parent_capability.try_clone()?,
        };
        let journal = album.open_directory_child(
            OsStr::new(".tonepoet-actions-manual"),
            true,
            0o700,
        )?;
        Ok((album, output, journal))
    }

    fn manual_authority_capability(
        &self,
        create: bool,
    ) -> Result<PinnedDirectoryCapability, ActionError> {
        let album = self.album_capability()?;
        Ok(album.open_directory_child(
            OsStr::new(".tonepoet-actions-manual"),
            create,
            0o700,
        )?)
    }

    fn manual_authority_capability_optional(
        &self,
    ) -> Result<Option<PinnedDirectoryCapability>, ActionError> {
        let album = self.album_capability()?;
        match album.open_directory_child(
            OsStr::new(".tonepoet-actions-manual"),
            false,
            0o700,
        ) {
            Ok(capability) => Ok(Some(capability)),
            Err(CapFsError::Io(error)) if error.raw_os_error() == Some(libc::ENOENT) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    pub(crate) fn canonical_album_dir(&self) -> &Path {
        &self.album_dir
    }

    /// Descriptor-namespace route to the exact retained publication parent.
    /// Appending `album_component` produces a path that remains bound to this
    /// authority even if the caller's original parent pathname is replaced.
    pub(crate) fn descriptor_relative_album_dir(&self) -> Result<PathBuf, ActionError> {
        Ok(self
            .parent_capability
            .descriptor_path()?
            .join(&self.album_component))
    }

    pub(crate) fn retained_publication_album(
        &self,
    ) -> Result<PinnedDirectoryCapability, ActionError> {
        self.album_capability()
    }

    pub(crate) fn retained_output_root(
        &self,
    ) -> Result<Option<PinnedDirectoryCapability>, ActionError> {
        self.output_root_capability
            .as_ref()
            .map(PinnedDirectoryCapability::try_clone)
            .transpose()
            .map_err(Into::into)
    }

    pub(crate) fn logical_output_root(&self) -> Option<&Path> {
        self.logical_output_root.as_deref()
    }

    pub(crate) fn descriptor_relative_output_root(&self) -> Result<Option<PathBuf>, ActionError> {
        self.output_root_capability
            .as_ref()
            .map(PinnedDirectoryCapability::descriptor_path)
            .transpose()
            .map_err(Into::into)
    }

    /// Release only the short-lived album publication transition authority.
    /// The separate action-execution lock, when present, remains held so manual
    /// and automatic actions cannot overlap after publication is committed.
    pub(crate) fn release_publication_authority(&mut self) {
        if self.publication_file.is_none() {
            return;
        }
        if let Err(error) = self.parent_capability.remove_regular_child_if_identity(
            &self.publication_lock_name,
            self.publication_lock_identity,
        ) {
            log::warn!(
                "could not remove shared album publication authority while still locked at {}: {error}",
                self.parent_capability
                    .display_path()
                    .join(&self.publication_lock_name)
                    .display()
            );
        }
        if let Some(file) = self.publication_file.take() {
            let _ = file.unlock();
            drop(file);
        }
    }

    #[allow(dead_code)] // bundle-provided API surface, not yet wired to a caller
    pub(crate) fn holds_action_execution_authority(&self) -> bool {
        self.action_file.is_some()
    }

    /// Check only the directory entry inside the retained parent.  Publication
    /// itself is rooted at `parent_capability.descriptor_path()`, so this is a
    /// contradiction check rather than a pathname check-then-use boundary.
    pub(crate) fn validate_prepared_publication_target_absent(&self) -> Result<(), ActionError> {
        if self
            .parent_capability
            .entry_identity(&self.album_component)?
            .is_some()
        {
            return Err(ActionError::Conflict(format!(
                "prepared multi-root publication target unexpectedly exists inside retained parent: {}",
                self.album_dir.display()
            )));
        }
        Ok(())
    }

    pub(crate) fn revalidate_publication_target(&self) -> Result<(), ActionError> {
        let current_album = self.parent_capability.entry_identity(&self.album_component)?;
        if current_album != self.album_identity_at_acquire {
            return Err(ActionError::Conflict(format!(
                "album publication target changed inside retained parent after authority acquisition: {}",
                self.album_dir.display()
            )));
        }
        Ok(())
    }
}

fn bind_explicit_context_to_lock(
    context: &ActionContext,
    lock: &ExplicitActionRunLock,
) -> Result<ActionContext, ActionError> {
    let mut bound = context.clone();
    let (album, output, journal) = lock.retained_manual_context_capabilities()?;
    for (label, existing, expected) in [
        (
            "album",
            bound.retained_album_capability.as_deref(),
            &album,
        ),
        (
            "output",
            bound.retained_output_capability.as_deref(),
            &output,
        ),
        (
            "journal",
            bound.retained_journal_capability.as_deref(),
            &journal,
        ),
    ] {
        if let Some(existing) = existing {
            if existing.identity() != expected.identity() {
                return Err(ActionError::Conflict(format!(
                    "explicit {label} context capability does not match the held manual action authority"
                )));
            }
        }
    }
    bound.retained_album_capability = Some(Arc::new(album));
    bound.retained_output_capability = Some(Arc::new(output));
    bound.retained_journal_capability = Some(Arc::new(journal));
    lock.validate_context(&bound)?;
    Ok(bound)
}

impl Drop for ExplicitActionRunLock {
    fn drop(&mut self) {
        // Remove every authoritative directory entry while its corresponding
        // descriptor is still exclusively locked. Waiters on an orphaned inode
        // then fail the descriptor-relative identity check and retry, instead of
        // splitting authority with a newcomer on a replacement inode.
        if self.action_file.is_some() {
            if let (Some(name), Some(identity)) =
                (self.action_lock_name.as_ref(), self.action_lock_identity)
            {
                if let Err(error) = self
                    .parent_capability
                    .remove_regular_child_if_identity(name, identity)
                {
                    log::warn!(
                        "could not remove shared action-execution authority while still locked at {}: {error}",
                        self.parent_capability.display_path().join(name).display()
                    );
                }
            }
        }
        if self.publication_file.is_some() {
            if let Err(error) = self.parent_capability.remove_regular_child_if_identity(
                &self.publication_lock_name,
                self.publication_lock_identity,
            ) {
                log::warn!(
                    "could not remove shared album publication authority while still locked at {}: {error}",
                    self.parent_capability
                        .display_path()
                        .join(&self.publication_lock_name)
                        .display()
                );
            }
        }
        if let Some(file) = self.action_file.take() {
            let _ = file.unlock();
            drop(file);
        }
        if let Some(file) = self.publication_file.take() {
            let _ = file.unlock();
            drop(file);
        }
        release_explicit_in_process_lock_key(&self.in_process_key);
    }
}

/// Legacy lock-file location. Older releases may have left this inert file in
/// the manual journal directory, so recovery scanning must continue to ignore
/// it. New releases use the shared album publication lock in the album parent.
fn explicit_action_run_lock_path(context: &ActionContext) -> PathBuf {
    context.journal_dir.join(".manual-run.lock")
}

fn explicit_active_run_path(context: &ActionContext) -> PathBuf {
    context.journal_dir.join(EXPLICIT_ACTIVE_RUN_FILE)
}

fn explicit_active_run_temporary_path(context: &ActionContext) -> PathBuf {
    context.journal_dir.join(EXPLICIT_ACTIVE_RUN_TEMP_FILE)
}

fn explicit_preview_path(context: &ActionContext) -> PathBuf {
    context.journal_dir.join(EXPLICIT_PREVIEW_FILE)
}

fn explicit_preview_temporary_path(context: &ActionContext) -> PathBuf {
    context.journal_dir.join(EXPLICIT_PREVIEW_TEMP_FILE)
}

fn shared_album_publication_lock_name(album_component: &OsStr) -> OsString {
    let mut name = OsString::from(".");
    name.push(album_component);
    name.push(".lock");
    name
}

fn shared_action_execution_lock_name(album_component: &OsStr) -> OsString {
    let mut name = OsString::from(".");
    name.push(album_component);
    name.push(".actions.lock");
    name
}

#[cfg(test)]
fn shared_album_publication_lock_display_path_for_test(
    album_dir: &Path,
) -> Result<PathBuf, ActionError> {
    let (parent, component, _) = open_album_parent_capability(album_dir)?;
    Ok(parent
        .display_path()
        .join(shared_album_publication_lock_name(&component)))
}

fn action_lock_contention(error: &io::Error) -> bool {
    if error.kind() == io::ErrorKind::WouldBlock {
        return true;
    }
    #[cfg(unix)]
    {
        return matches!(error.raw_os_error(), Some(11) | Some(35));
    }
    #[cfg(not(unix))]
    {
        false
    }
}

fn acquire_current_descriptor_child_lock(
    parent: &PinnedDirectoryCapability,
    name: &OsStr,
    blocking: bool,
    busy_message: &str,
) -> Result<(File, CapEntryIdentity), ActionError> {
    loop {
        let file = parent.open_regular_child(name, true, 0o600)?;
        let lock_result = if blocking {
            file.lock_exclusive()
        } else {
            file.try_lock_exclusive()
        };
        match lock_result {
            Ok(()) => {}
            Err(error) if !blocking && action_lock_contention(&error) => {
                return Err(ActionError::Conflict(busy_message.to_string()));
            }
            Err(error) => return Err(error.into()),
        }

        let held = metadata_for_open_file(&file)?.entry_identity();
        let current = parent.entry_identity(name)?;
        if current != Some(held) {
            let _ = file.unlock();
            drop(file);
            continue;
        }
        return Ok((file, held));
    }
}

fn remove_and_unlock_descriptor_child_best_effort(
    parent: &PinnedDirectoryCapability,
    name: &OsStr,
    identity: CapEntryIdentity,
    file: File,
) {
    if let Err(error) = parent.remove_regular_child_if_identity(name, identity) {
        log::warn!(
            "could not remove partially acquired authority while still locked at {}: {error}",
            parent.display_path().join(name).display()
        );
    }
    let _ = file.unlock();
    drop(file);
}

fn acquire_shared_album_publication_lock_from_opened(
    parent_capability: PinnedDirectoryCapability,
    album_component: OsString,
    canonical_album_dir: PathBuf,
    output_root_capability: Option<PinnedDirectoryCapability>,
    logical_output_root: Option<PathBuf>,
    blocking: bool,
    include_action_execution: bool,
) -> Result<ExplicitActionRunLock, ActionError> {
    let parent_identity = parent_capability.identity();
    let in_process_key = ExplicitInProcessAuthorityKey {
        parent_device: parent_identity.device,
        parent_inode: parent_identity.inode,
        album_component: album_component.clone(),
    };

    // Blocking acquirers (ordinary album publishers) WAIT for the in-process
    // key like they wait for the file lock — concurrent same-process
    // publishes into one album must serialize, not error. Non-blocking
    // acquirers (interactive :actions-run et al) fail fast. The registry
    // mutex is NOT held across waits: sleeping under it would stall
    // acquisitions for unrelated albums.
    let _process_registry = {
        let mut waited = std::time::Duration::ZERO;
        loop {
            let registry = explicit_lock_registry_mutex()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let inserted = {
                let mut keys = explicit_in_process_lock_keys()
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                keys.insert(in_process_key.clone())
            };
            if inserted {
                break registry;
            }
            drop(registry);
            if !blocking {
                return Err(ActionError::Conflict(format!(
                    "another :actions-run, identity import, post-action executor, or album publisher currently owns authority for {}",
                    canonical_album_dir.display()
                )));
            }
            if waited >= std::time::Duration::from_secs(600) {
                return Err(ActionError::Conflict(format!(
                    "timed out waiting for in-process album authority for {}",
                    canonical_album_dir.display()
                )));
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
            waited += std::time::Duration::from_millis(25);
        }
    };

    let acquisition = (|| -> Result<ExplicitActionRunLock, ActionError> {
        let publication_lock_name = shared_album_publication_lock_name(&album_component);
        let busy_message = format!(
            "another :actions-run, identity import, post-action executor, or album publisher currently owns publication authority for {}",
            canonical_album_dir.display()
        );
        let (publication_file, publication_lock_identity) =
            acquire_current_descriptor_child_lock(
                &parent_capability,
                &publication_lock_name,
                blocking,
                &busy_message,
            )?;

        let (action_file, action_lock_name, action_lock_identity) =
            if include_action_execution {
                let action_name = shared_action_execution_lock_name(&album_component);
                let action_busy = format!(
                    "another automatic or manual action executor currently owns {}",
                    canonical_album_dir.display()
                );
                match acquire_current_descriptor_child_lock(
                    &parent_capability,
                    &action_name,
                    blocking,
                    &action_busy,
                ) {
                    Ok((file, identity)) => (Some(file), Some(action_name), Some(identity)),
                    Err(error) => {
                        remove_and_unlock_descriptor_child_best_effort(
                            &parent_capability,
                            &publication_lock_name,
                            publication_lock_identity,
                            publication_file,
                        );
                        return Err(error);
                    }
                }
            } else {
                (None, None, None)
            };

        let album_identity_at_acquire = parent_capability.entry_identity(&album_component)?;
        Ok(ExplicitActionRunLock {
            publication_file: Some(publication_file),
            publication_lock_name,
            publication_lock_identity,
            action_file,
            action_lock_name,
            action_lock_identity,
            album_dir: canonical_album_dir.clone(),
            album_component: album_component.clone(),
            journal_dir: canonical_album_dir.join(".tonepoet-actions-manual"),
            parent_capability,
            output_root_capability,
            logical_output_root,
            album_identity_at_acquire,
            in_process_key: in_process_key.clone(),
        })
    })();

    if acquisition.is_err() {
        release_explicit_in_process_lock_key(&in_process_key);
    }
    acquisition
}

fn acquire_shared_album_publication_lock(
    album_dir: &Path,
    blocking: bool,
    include_action_execution: bool,
) -> Result<ExplicitActionRunLock, ActionError> {
    let (parent_capability, album_component, canonical_album_dir) =
        open_album_parent_capability(album_dir)?;
    acquire_shared_album_publication_lock_from_opened(
        parent_capability,
        album_component,
        canonical_album_dir,
        None,
        None,
        blocking,
        include_action_execution,
    )
}

fn acquire_shared_album_publication_lock_under_output_capability(
    album_dir: &Path,
    logical_output_root: &Path,
    output_root_capability: &PinnedDirectoryCapability,
    blocking: bool,
    include_action_execution: bool,
) -> Result<ExplicitActionRunLock, ActionError> {
    let logical_output_root = lexical_normalize_absolute(logical_output_root)?;
    let (parent_capability, album_component, canonical_album_dir) =
        open_album_parent_capability_under_output_root(
            album_dir,
            &logical_output_root,
            output_root_capability,
        )?;
    acquire_shared_album_publication_lock_from_opened(
        parent_capability,
        album_component,
        canonical_album_dir,
        Some(output_root_capability.try_clone()?),
        Some(logical_output_root),
        blocking,
        include_action_execution,
    )
}

fn acquire_shared_album_publication_lock_in_output_root(
    album_dir: &Path,
    output_root: &Path,
    blocking: bool,
    include_action_execution: bool,
) -> Result<ExplicitActionRunLock, ActionError> {
    let logical_output_root = lexical_normalize_absolute(output_root)?;
    fs::create_dir_all(&logical_output_root)?;
    let output_root_capability = PinnedDirectoryCapability::open_trusted(&logical_output_root)?;
    acquire_shared_album_publication_lock_under_output_capability(
        album_dir,
        &logical_output_root,
        &output_root_capability,
        blocking,
        include_action_execution,
    )
}

pub(crate) fn acquire_explicit_action_run_lock_for_album(
    album_dir: &Path,
) -> Result<ExplicitActionRunLock, ActionError> {
    acquire_shared_album_publication_lock(album_dir, false, true)
}

pub(crate) fn acquire_explicit_action_run_lock_for_album_in_output_root(
    album_dir: &Path,
    output_root: &Path,
) -> Result<ExplicitActionRunLock, ActionError> {
    acquire_shared_album_publication_lock_in_output_root(
        album_dir,
        output_root,
        false,
        true,
    )
}

pub(crate) fn acquire_explicit_action_run_lock_for_album_under_output_capability(
    album_dir: &Path,
    logical_output_root: &Path,
    output_root_capability: &PinnedDirectoryCapability,
) -> Result<ExplicitActionRunLock, ActionError> {
    acquire_shared_album_publication_lock_under_output_capability(
        album_dir,
        logical_output_root,
        output_root_capability,
        false,
        true,
    )
}

pub(crate) fn acquire_blocking_album_publication_lock_for_album(
    album_dir: &Path,
) -> Result<ExplicitActionRunLock, ActionError> {
    acquire_shared_album_publication_lock(album_dir, true, false)
}

/// Blocking variants WITH action-execution authority: pipeline publishes of
/// action-enabled albums run per-track under a shared worker pool and must
/// serialize on the album (like the plain publication lock) rather than fail
/// fast like the interactive :actions-run acquisitions.
pub(crate) fn acquire_blocking_action_run_lock_for_album_in_output_root(
    album_dir: &Path,
    output_root: &Path,
) -> Result<ExplicitActionRunLock, ActionError> {
    acquire_shared_album_publication_lock_in_output_root(album_dir, output_root, true, true)
}

pub(crate) fn acquire_blocking_action_run_lock_for_album_under_output_capability(
    album_dir: &Path,
    logical_output_root: &Path,
    output_root_capability: &PinnedDirectoryCapability,
) -> Result<ExplicitActionRunLock, ActionError> {
    acquire_shared_album_publication_lock_under_output_capability(
        album_dir,
        logical_output_root,
        output_root_capability,
        true,
        true,
    )
}

pub(crate) fn acquire_blocking_album_publication_lock_for_album_in_output_root(
    album_dir: &Path,
    output_root: &Path,
) -> Result<ExplicitActionRunLock, ActionError> {
    acquire_shared_album_publication_lock_in_output_root(
        album_dir,
        output_root,
        true,
        false,
    )
}

pub(crate) fn acquire_blocking_album_publication_lock_for_album_under_output_capability(
    album_dir: &Path,
    logical_output_root: &Path,
    output_root_capability: &PinnedDirectoryCapability,
) -> Result<ExplicitActionRunLock, ActionError> {
    acquire_shared_album_publication_lock_under_output_capability(
        album_dir,
        logical_output_root,
        output_root_capability,
        true,
        false,
    )
}

fn manual_authority_capability_has_unresolved_state(
    authority: &PinnedDirectoryCapability,
) -> bool {
    let entries = match authority.list_entries() {
        Ok(entries) => entries,
        Err(_) => return true,
    };
    for (name, identity) in entries {
        let display_path = authority.display_path().join(&name);
        if identity.file_type == CapFileType::Symlink {
            return true;
        }
        let name_text = name.to_string_lossy();
        if matches!(
            name_text.as_ref(),
            EXPLICIT_ACTIVE_RUN_FILE
                | EXPLICIT_ACTIVE_RUN_TEMP_FILE
                | EXPLICIT_PREVIEW_FILE
                | EXPLICIT_PREVIEW_TEMP_FILE
        ) {
            return true;
        }
        if name_text == ".manual-run.lock" && identity.file_type == CapFileType::Regular {
            continue;
        }
        if name_text.starts_with("actions-") && name_text.ends_with(".journal.json") {
            if identity.file_type != CapFileType::Regular {
                return true;
            }
            let bytes = match authority.read_regular_child_optional(&name) {
                Ok(Some((bytes, observed))) if observed == identity => bytes,
                _ => return true,
            };
            let journal = match deserialize_action_journal(&bytes) {
                Ok(journal) => journal,
                Err(_) => return true,
            };
            if validate_terminal_journal_authority_without_write_temporary(
                &journal,
                &display_path,
            )
            .is_err()
            {
                return true;
            }
            continue;
        }
        if name_text.starts_with("actions-") && name_text.ends_with(".result.json") {
            if identity.file_type != CapFileType::Regular {
                return true;
            }
            let bytes = match authority.read_regular_child_optional(&name) {
                Ok(Some((bytes, observed))) if observed == identity => bytes,
                _ => return true,
            };
            let result = match serde_json::from_slice::<ActionResultRecord>(&bytes) {
                Ok(result) => result,
                Err(_) => return true,
            };
            match read_valid_result_bytes(&bytes, &result.election, None) {
                Ok(report) if !report.recovery_required => continue,
                _ => return true,
            }
        }
        // Claims, write temporaries, witnesses, workspaces, unknown files, and
        // every subdirectory are forward-compatible recovery authority.
        return true;
    }
    false
}

pub(crate) fn ensure_album_capability_has_no_unresolved_explicit_state(
    album: &PinnedDirectoryCapability,
) -> Result<(), ActionError> {
    let authority = match album.open_directory_child(
        OsStr::new(".tonepoet-actions-manual"),
        false,
        0o700,
    ) {
        Ok(authority) => authority,
        Err(CapFsError::Io(error)) if error.raw_os_error() == Some(libc::ENOENT) => {
            return Ok(())
        }
        Err(error) => return Err(error.into()),
    };
    if manual_authority_capability_has_unresolved_state(&authority) {
        return Err(ActionError::Conflict(format!(
            "album mutation refuses unresolved or unreadable manual action recovery authority at {}",
            authority.display_path().display()
        )));
    }
    Ok(())
}

pub(crate) fn ensure_album_publication_has_no_unresolved_explicit_state(
    lock: &ExplicitActionRunLock,
    album_dir: &Path,
) -> Result<(), ActionError> {
    lock.validate_album_dir(album_dir)?;
    let album = lock.album_capability()?;
    ensure_album_capability_has_no_unresolved_explicit_state(&album)
}


/// Actionless conversions use this read-only probe only when an authority
/// artifact already exists. It never creates a lock, directory, sidecar, or
/// journal and deliberately does not impose action-only path traversal rules
/// on ordinary publication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // bundle-provided API surface, not yet wired to a caller
pub(crate) struct ExistingExplicitAuthority {
    pub identity_sidecar_present: bool,
    pub authority_present: bool,
}

/// Inspect pre-existing manual/action identity authority without creating any
/// filesystem object. The result lets actionless publication retain the normal
/// artifact-free path when no authority exists, while serializing against an
/// album that has opted into later manual actions.
#[allow(dead_code)] // bundle-provided API surface, not yet wired to a caller
pub(crate) fn inspect_existing_explicit_authority_without_creating(
    album_dir: &Path,
) -> Result<ExistingExplicitAuthority, ActionError> {
    let album_dir = lexical_normalize_absolute(album_dir)?;
    let identity_path = album_dir.join(".tonepoet-action-identity.json");
    let identity_sidecar_present = match fs::symlink_metadata(&identity_path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(ActionError::Conflict(format!(
                "canonical identity authority has an unsafe type: {}",
                identity_path.display()
            )));
        }
        Ok(_) => true,
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => return Err(error.into()),
    };

    let identity_temporary = album_dir.join(".tonepoet-action-identity.write.tmp");
    match fs::symlink_metadata(&identity_temporary) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(ActionError::Conflict(format!(
                "canonical identity recovery authority has an unsafe type: {}",
                identity_temporary.display()
            )));
        }
        Ok(_) => {
            return Err(ActionError::Conflict(format!(
                "album publication conflicts with unresolved canonical identity authority at {}",
                identity_temporary.display()
            )));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    let manual_root = album_dir.join(".tonepoet-actions-manual");
    let manual_authority_present = match fs::symlink_metadata(&manual_root) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(ActionError::Conflict(format!(
                "manual action recovery authority has an unsafe type: {}",
                manual_root.display()
            )));
        }
        Ok(_) if workspace_has_unresolved_action_state(&manual_root) => {
            return Err(ActionError::Conflict(format!(
                "album publication conflicts with unresolved manual action state at {}",
                manual_root.display()
            )));
        }
        Ok(_) => true,
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => return Err(error.into()),
    };

    Ok(ExistingExplicitAuthority {
        identity_sidecar_present,
        authority_present: identity_sidecar_present || manual_authority_present,
    })
}

#[allow(dead_code)] // bundle-provided API surface, not yet wired to a caller
pub(crate) fn ensure_no_active_explicit_mutation_without_creating(
    album_dir: &Path,
) -> Result<(), ActionError> {
    inspect_existing_explicit_authority_without_creating(album_dir).map(|_| ())
}

fn acquire_explicit_action_run_lock(
    context: &ActionContext,
) -> Result<ExplicitActionRunLock, ActionError> {
    // Carry the context's real output root: the manual-context "output"
    // capability must identify context.output_root, not the album's
    // immediate parent (they differ under artist/album nesting).
    let lock = acquire_explicit_action_run_lock_for_album_in_output_root(
        &context.album_dir,
        &context.output_root,
    )?;
    lock.validate_context(context)?;
    Ok(lock)
}

fn explicit_active_run_record(
    pipeline: &ActionPipeline,
    context: &ActionContext,
    run_identity: String,
    preview_authority_sha256: Option<String>,
) -> Result<ExplicitActiveRunRecord, ActionError> {
    if run_identity != context.run_identity {
        return Err(ActionError::Conflict(
            "explicit active-run identity does not match its execution context".to_string(),
        ));
    }
    let pipeline_serialized = pipeline.canonical_serialization()?;
    let pipeline_sha256 = sha256_hex(pipeline_serialized.as_bytes());
    let mut run_context = context.clone();
    run_context.run_identity = run_identity.clone();
    let payload = ExplicitActiveRunPayload {
        schema_version: EXPLICIT_ACTIVE_RUN_SCHEMA_VERSION,
        album_dir: context.album_dir.clone(),
        album_identity: context.album_identity.clone(),
        phase: context.phase,
        pipeline_serialized,
        pipeline_sha256: pipeline_sha256.clone(),
        run_identity,
        journal_path: action_journal_path(&run_context, &pipeline_sha256)?,
        preview_authority_sha256,
        created_unix_nanos: now_unix_nanos(),
    };
    let payload_bytes = serde_json::to_vec(&payload).map_err(ActionError::Serialization)?;
    Ok(ExplicitActiveRunRecord {
        payload,
        payload_sha256: sha256_hex(&payload_bytes),
    })
}

fn validate_explicit_active_run_record(
    record: &ExplicitActiveRunRecord,
) -> Result<(), ActionError> {
    if record.payload.schema_version != EXPLICIT_ACTIVE_RUN_SCHEMA_VERSION {
        return Err(ActionError::InvalidJournal(format!(
            "unsupported explicit active-run schema {}",
            record.payload.schema_version
        )));
    }
    let payload_bytes = serde_json::to_vec(&record.payload).map_err(ActionError::Serialization)?;
    if sha256_hex(&payload_bytes) != record.payload_sha256 {
        return Err(ActionError::InvalidJournal(
            "explicit active-run payload checksum mismatch".to_string(),
        ));
    }
    if let Some(checksum) = record.payload.preview_authority_sha256.as_deref() {
        if checksum.len() != 64 || !checksum.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(ActionError::InvalidJournal(
                "explicit active-run preview authority checksum is invalid".to_string(),
            ));
        }
    }
    if !record.payload.run_identity.starts_with("manual-invocation:")
        && !record.payload.run_identity.starts_with("manual-published:")
    {
        return Err(ActionError::InvalidJournal(
            "explicit active-run identity has an unsupported namespace".to_string(),
        ));
    }
    if record.payload.run_identity.starts_with("manual-invocation:") {
        let uuid = record
            .payload
            .run_identity
            .trim_start_matches("manual-invocation:");
        Uuid::parse_str(uuid).map_err(|_| {
            ActionError::InvalidJournal(
                "explicit active-run invocation identity is not a UUID".to_string(),
            )
        })?;
    } else {
        let digest = record
            .payload
            .run_identity
            .trim_start_matches("manual-published:");
        if digest.len() != 64 || !digest.as_bytes().iter().all(u8::is_ascii_hexdigit) {
            return Err(ActionError::InvalidJournal(
                "legacy explicit active-run identity does not contain a canonical SHA-256"
                    .to_string(),
            ));
        }
    }
    if sha256_hex(record.payload.pipeline_serialized.as_bytes())
        != record.payload.pipeline_sha256
    {
        return Err(ActionError::InvalidJournal(
            "explicit active-run pipeline digest mismatch".to_string(),
        ));
    }
    Ok(())
}

fn validate_explicit_active_run_scope(
    record: &ExplicitActiveRunRecord,
    context: &ActionContext,
) -> Result<ActionPipeline, ActionError> {
    validate_explicit_active_run_record(record)?;
    if record.payload.album_dir != context.album_dir
        || record.payload.album_identity != context.album_identity
        || record.payload.phase != context.phase
    {
        return Err(ActionError::Conflict(format!(
            "an unresolved :actions-run authority belongs to a different canonical album identity or phase: {}",
            explicit_active_run_path(context).display()
        )));
    }
    let pipeline: ActionPipeline = serde_json::from_str(&record.payload.pipeline_serialized)
        .map_err(ActionError::Serialization)?;
    let serialized = pipeline.canonical_serialization()?;
    let digest = sha256_hex(serialized.as_bytes());
    if serialized != record.payload.pipeline_serialized
        || digest != record.payload.pipeline_sha256
    {
        return Err(ActionError::InvalidJournal(
            "explicit active-run pipeline is not canonically serialized".to_string(),
        ));
    }
    let mut run_context = context.clone();
    run_context.run_identity = record.payload.run_identity.clone();
    let expected_path = action_journal_path(&run_context, &digest)?;
    if record.payload.journal_path != expected_path {
        return Err(ActionError::InvalidJournal(
            "explicit active-run journal path does not match its immutable identity".to_string(),
        ));
    }
    Ok(pipeline)
}

fn validate_explicit_active_run_pipeline_matches(
    record: &ExplicitActiveRunRecord,
    pipeline: &ActionPipeline,
) -> Result<(), ActionError> {
    let serialized = pipeline.canonical_serialization()?;
    if record.payload.pipeline_serialized != serialized
        || record.payload.pipeline_sha256 != sha256_hex(serialized.as_bytes())
    {
        return Err(ActionError::Conflict(format!(
            "an unresolved :actions-run authority belongs to a different action pipeline: {}",
            record.payload.journal_path.display()
        )));
    }
    Ok(())
}


fn load_explicit_active_run_locked(
    context: &ActionContext,
    lock: &ExplicitActionRunLock,
) -> Result<Option<(ExplicitActiveRunRecord, Vec<u8>)>, ActionError> {
    lock.validate_context(context)?;
    let Some(authority) = lock.manual_authority_capability_optional()? else {
        return Ok(None);
    };
    let final_file = authority.read_regular_child_optional(OsStr::new(EXPLICIT_ACTIVE_RUN_FILE))?;
    let temporary_file =
        authority.read_regular_child_optional(OsStr::new(EXPLICIT_ACTIVE_RUN_TEMP_FILE))?;

    let parse = |bytes: Vec<u8>| -> Result<(ExplicitActiveRunRecord, Vec<u8>), ActionError> {
        let record: ExplicitActiveRunRecord =
            serde_json::from_slice(&bytes).map_err(ActionError::Serialization)?;
        validate_explicit_active_run_record(&record)?;
        Ok((record, bytes))
    };

    match (final_file, temporary_file) {
        (None, None) => Ok(None),
        (Some((bytes, _)), None) => parse(bytes).map(Some),
        (None, Some((bytes, temporary_identity))) => {
            let parsed = parse(bytes)?;
            authority.publish_regular_child_no_clobber(
                OsStr::new(EXPLICIT_ACTIVE_RUN_TEMP_FILE),
                OsStr::new(EXPLICIT_ACTIVE_RUN_FILE),
                temporary_identity,
            )?;
            Ok(Some(parsed))
        }
        (Some((final_bytes, _final_identity)), Some((temporary_bytes, temporary_identity))) => {
            let final_parsed = parse(final_bytes)?;
            let temporary_parsed = parse(temporary_bytes)?;
            if final_parsed.0 != temporary_parsed.0 {
                return Err(ActionError::Contradiction(
                    "explicit active-run final and write-temporary disagree".to_string(),
                ));
            }
            authority.remove_regular_child_if_identity(
                OsStr::new(EXPLICIT_ACTIVE_RUN_TEMP_FILE),
                temporary_identity,
            )?;
            Ok(Some(final_parsed))
        }
    }
}

fn load_explicit_active_run_for_context_locked(
    context: &ActionContext,
    lock: &ExplicitActionRunLock,
) -> Result<Option<(ExplicitActiveRunRecord, ActionPipeline, ActionContext)>, ActionError> {
    let Some((record, _bytes)) = load_explicit_active_run_locked(context, lock)? else {
        return Ok(None);
    };
    let pipeline = validate_explicit_active_run_scope(&record, context)?;
    let mut active_context = context.clone();
    active_context.run_identity = record.payload.run_identity.clone();
    Ok(Some((record, pipeline, active_context)))
}

fn create_explicit_active_run_locked(
    pipeline: &ActionPipeline,
    context: &ActionContext,
    run_identity: String,
    preview_authority_sha256: Option<String>,
    lock: &ExplicitActionRunLock,
) -> Result<ExplicitActiveRunRecord, ActionError> {
    lock.validate_context(context)?;
    let authority = lock.manual_authority_capability(true)?;
    if load_explicit_active_run_locked(context, lock)?.is_some() {
        return Err(ActionError::Conflict(
            "explicit active-run authority appeared while allocating a new invocation".to_string(),
        ));
    }
    let record = explicit_active_run_record(
        pipeline,
        context,
        run_identity,
        preview_authority_sha256,
    )?;
    let bytes = serde_json::to_vec_pretty(&record).map_err(ActionError::Serialization)?;
    let temporary_identity = authority.write_regular_child_create_new_durable(
        OsStr::new(EXPLICIT_ACTIVE_RUN_TEMP_FILE),
        &bytes,
        0o600,
    )?;
    if let Err(error) = authority.publish_regular_child_no_clobber(
        OsStr::new(EXPLICIT_ACTIVE_RUN_TEMP_FILE),
        OsStr::new(EXPLICIT_ACTIVE_RUN_FILE),
        temporary_identity,
    ) {
        let _ = authority.remove_regular_child_if_identity(
            OsStr::new(EXPLICIT_ACTIVE_RUN_TEMP_FILE),
            temporary_identity,
        );
        return Err(error.into());
    }
    Ok(record)
}

fn remove_explicit_active_run_locked(
    record: &ExplicitActiveRunRecord,
    context: &ActionContext,
    lock: &ExplicitActionRunLock,
) -> Result<(), ActionError> {
    lock.validate_context(context)?;
    let authority = lock.manual_authority_capability(false)?;
    let (bytes, identity) = authority
        .read_regular_child_optional(OsStr::new(EXPLICIT_ACTIVE_RUN_FILE))?
        .ok_or_else(|| {
            ActionError::Contradiction(
                "explicit active-run authority disappeared before retirement".to_string(),
            )
        })?;
    let current: ExplicitActiveRunRecord =
        serde_json::from_slice(&bytes).map_err(ActionError::Serialization)?;
    if &current != record {
        return Err(ActionError::Contradiction(
            "explicit active-run authority changed before retirement".to_string(),
        ));
    }
    authority.remove_regular_child_if_identity(
        OsStr::new(EXPLICIT_ACTIVE_RUN_FILE),
        identity,
    )?;
    Ok(())
}

fn validate_explicit_preview_record(record: &ExplicitPreviewRecord) -> Result<(), ActionError> {
    if record.payload.schema_version != EXPLICIT_PREVIEW_SCHEMA_VERSION {
        return Err(ActionError::InvalidJournal(format!(
            "unsupported explicit preview schema {}",
            record.payload.schema_version
        )));
    }
    let payload_bytes = serde_json::to_vec(&record.payload).map_err(ActionError::Serialization)?;
    if sha256_hex(&payload_bytes) != record.payload_sha256 {
        return Err(ActionError::InvalidJournal(
            "explicit preview authority checksum mismatch".to_string(),
        ));
    }
    if !record.payload.invocation_id.starts_with("manual-invocation:") {
        return Err(ActionError::InvalidJournal(
            "explicit preview invocation identity is invalid".to_string(),
        ));
    }
    if record.payload.claim_id.is_empty()
        || record.payload.pipeline_sha256.len() != 64
        || record.payload.plans_sha256.len() != 64
        || sha256_hex(record.payload.pipeline_serialized.as_bytes())
            != record.payload.pipeline_sha256
        || sha256_hex(record.payload.plans_serialized.as_bytes()) != record.payload.plans_sha256
    {
        return Err(ActionError::InvalidJournal(
            "explicit preview authority digest validation failed".to_string(),
        ));
    }
    let pipeline: ActionPipeline = serde_json::from_str(&record.payload.pipeline_serialized)
        .map_err(ActionError::Serialization)?;
    if pipeline.canonical_serialization()? != record.payload.pipeline_serialized {
        return Err(ActionError::InvalidJournal(
            "explicit preview pipeline is not canonically serialized".to_string(),
        ));
    }
    let plans: Vec<ActionPlan> = serde_json::from_str(&record.payload.plans_serialized)
        .map_err(ActionError::Serialization)?;
    let configured = pipeline.for_phase(record.payload.phase);
    if plans.len() != configured.len()
        || plans
            .iter()
            .zip(configured)
            .any(|(plan, action)| plan.action_kind != action.kind_name())
    {
        return Err(ActionError::InvalidJournal(
            "explicit preview plans do not correspond to the configured pipeline".to_string(),
        ));
    }
    if record.payload.subject_entry_identity.kind != PreviewEntryKind::Directory {
        return Err(ActionError::InvalidJournal(
            "explicit preview subject identity is not a directory".to_string(),
        ));
    }
    let mut previous_tree_path: Option<&Path> = None;
    for entry in &record.payload.matcher_tree {
        if previous_tree_path.is_some_and(|previous| previous >= entry.path.as_path()) {
            return Err(ActionError::InvalidJournal(
                "explicit preview matcher tree is not strictly ordered".to_string(),
            ));
        }
        previous_tree_path = Some(&entry.path);
    }
    let mut graph_paths = BTreeSet::new();
    let mut graph_identities = BTreeSet::new();
    for object in &record.payload.operand_graph.objects {
        if !graph_identities.insert(object.entry_identity) || object.paths.is_empty() {
            return Err(ActionError::InvalidJournal(
                "explicit preview operand graph has duplicate or pathless objects".to_string(),
            ));
        }
        if let Some(identity) = object.expected_content.as_ref() {
            validate_object_identity(identity)?;
        }
        for path_roles in &object.paths {
            if path_roles.roles.is_empty() || !graph_paths.insert(path_roles.path.clone()) {
                return Err(ActionError::InvalidJournal(
                    "explicit preview operand graph has duplicate paths or empty roles".to_string(),
                ));
            }
        }
    }
    for absent in &record.payload.operand_graph.absent_paths {
        if absent.roles.is_empty() || !graph_paths.insert(absent.path.clone()) {
            return Err(ActionError::InvalidJournal(
                "explicit preview absent-path graph conflicts with an object path".to_string(),
            ));
        }
    }
    Ok(())
}

fn parse_explicit_preview(bytes: &[u8]) -> Result<ExplicitPreviewRecord, ActionError> {
    let record: ExplicitPreviewRecord =
        serde_json::from_slice(bytes).map_err(ActionError::Serialization)?;
    validate_explicit_preview_record(&record)?;
    Ok(record)
}

fn load_explicit_preview_locked(
    context: &ActionContext,
    lock: &ExplicitActionRunLock,
) -> Result<Option<(ExplicitPreviewRecord, CapEntryIdentity)>, ActionError> {
    lock.validate_context(context)?;
    let Some(authority) = lock.manual_authority_capability_optional()? else {
        return Ok(None);
    };
    let final_record = authority.read_regular_child_optional(OsStr::new(EXPLICIT_PREVIEW_FILE))?;
    let temporary_record =
        authority.read_regular_child_optional(OsStr::new(EXPLICIT_PREVIEW_TEMP_FILE))?;
    match (final_record, temporary_record) {
        (None, None) => Ok(None),
        (Some((bytes, identity)), None) => Ok(Some((parse_explicit_preview(&bytes)?, identity))),
        (None, Some((bytes, temporary_identity))) => {
            let record = parse_explicit_preview(&bytes)?;
            authority.publish_regular_child_no_clobber(
                OsStr::new(EXPLICIT_PREVIEW_TEMP_FILE),
                OsStr::new(EXPLICIT_PREVIEW_FILE),
                temporary_identity,
            )?;
            let identity = authority
                .entry_identity(OsStr::new(EXPLICIT_PREVIEW_FILE))?
                .ok_or_else(|| ActionError::Contradiction(
                    "explicit preview authority vanished after reconciliation".to_string(),
                ))?;
            Ok(Some((record, identity)))
        }
        (Some((final_bytes, final_identity)), Some((temporary_bytes, temporary_identity))) => {
            let final_record = parse_explicit_preview(&final_bytes)?;
            let temporary_record = parse_explicit_preview(&temporary_bytes)?;
            if final_record == temporary_record {
                authority.remove_regular_child_if_identity(
                    OsStr::new(EXPLICIT_PREVIEW_TEMP_FILE),
                    temporary_identity,
                )?;
                return Ok(Some((final_record, final_identity)));
            }
            if explicit_preview_binding_sha256(&final_record.payload)?
                != explicit_preview_binding_sha256(&temporary_record.payload)?
            {
                return Err(ActionError::Contradiction(
                    "explicit preview final and write-temporary belong to different invocations"
                        .to_string(),
                ));
            }
            if temporary_record.payload.generation
                == final_record.payload.generation.saturating_add(1)
            {
                authority.replace_regular_child(
                    OsStr::new(EXPLICIT_PREVIEW_TEMP_FILE),
                    OsStr::new(EXPLICIT_PREVIEW_FILE),
                    temporary_identity,
                    Some(final_identity),
                )?;
                let identity = authority
                    .entry_identity(OsStr::new(EXPLICIT_PREVIEW_FILE))?
                    .ok_or_else(|| ActionError::Contradiction(
                        "explicit preview authority vanished after generation reconciliation"
                            .to_string(),
                    ))?;
                return Ok(Some((temporary_record, identity)));
            }
            if final_record.payload.generation > temporary_record.payload.generation {
                authority.remove_regular_child_if_identity(
                    OsStr::new(EXPLICIT_PREVIEW_TEMP_FILE),
                    temporary_identity,
                )?;
                return Ok(Some((final_record, final_identity)));
            }
            Err(ActionError::Contradiction(
                "explicit preview authority generations are not adjacent".to_string(),
            ))
        }
    }
}

fn write_explicit_preview_locked(
    record: &ExplicitPreviewRecord,
    expected_previous: Option<&ExplicitPreviewRecord>,
    context: &ActionContext,
    lock: &ExplicitActionRunLock,
) -> Result<(), ActionError> {
    validate_explicit_preview_record(record)?;
    lock.validate_context(context)?;
    let authority = lock.manual_authority_capability(true)?;
    if let Some((_, temporary_identity)) = authority
        .read_regular_child_optional(OsStr::new(EXPLICIT_PREVIEW_TEMP_FILE))?
    {
        authority.remove_regular_child_if_identity(
            OsStr::new(EXPLICIT_PREVIEW_TEMP_FILE),
            temporary_identity,
        )?;
    }
    let current = authority.read_regular_child_optional(OsStr::new(EXPLICIT_PREVIEW_FILE))?;
    match (&current, expected_previous) {
        (None, None) => {}
        (Some((bytes, _)), Some(expected)) if parse_explicit_preview(bytes)? == *expected => {}
        _ => {
            return Err(ActionError::Contradiction(
                "explicit preview authority changed before update".to_string(),
            ));
        }
    }
    let bytes = serde_json::to_vec_pretty(record).map_err(ActionError::Serialization)?;
    let temporary_identity = authority.write_regular_child_create_new_durable(
        OsStr::new(EXPLICIT_PREVIEW_TEMP_FILE),
        &bytes,
        0o600,
    )?;
    match current {
        Some((_, final_identity)) => authority.replace_regular_child(
            OsStr::new(EXPLICIT_PREVIEW_TEMP_FILE),
            OsStr::new(EXPLICIT_PREVIEW_FILE),
            temporary_identity,
            Some(final_identity),
        )?,
        None => authority.publish_regular_child_no_clobber(
            OsStr::new(EXPLICIT_PREVIEW_TEMP_FILE),
            OsStr::new(EXPLICIT_PREVIEW_FILE),
            temporary_identity,
        )?,
    }
    Ok(())
}

fn remove_explicit_preview_locked(
    expected: &ExplicitPreviewRecord,
    context: &ActionContext,
    lock: &ExplicitActionRunLock,
) -> Result<(), ActionError> {
    lock.validate_context(context)?;
    let authority = lock.manual_authority_capability(false)?;
    let Some((bytes, identity)) =
        authority.read_regular_child_optional(OsStr::new(EXPLICIT_PREVIEW_FILE))?
    else {
        return Ok(());
    };
    if parse_explicit_preview(&bytes)? != *expected {
        return Err(ActionError::Contradiction(
            "explicit preview authority changed before removal".to_string(),
        ));
    }
    authority.remove_regular_child_if_identity(OsStr::new(EXPLICIT_PREVIEW_FILE), identity)?;
    Ok(())
}

fn explicit_preview_binding_sha256(
    payload: &ExplicitPreviewPayload,
) -> Result<String, ActionError> {
    let mut immutable = payload.clone();
    immutable.generation = 0;
    immutable.state = ManualInvocationState::PreviewPrepared;
    immutable.stale_reason = None;
    let bytes = serde_json::to_vec(&immutable).map_err(ActionError::Serialization)?;
    Ok(sha256_hex(&bytes))
}

fn explicit_preview_record(payload: ExplicitPreviewPayload) -> Result<ExplicitPreviewRecord, ActionError> {
    let payload_bytes = serde_json::to_vec(&payload).map_err(ActionError::Serialization)?;
    Ok(ExplicitPreviewRecord {
        payload,
        payload_sha256: sha256_hex(&payload_bytes),
    })
}

fn preview_entry_identity(
    filesystem: &dyn ActionFilesystem,
    path: &Path,
    role: &str,
) -> Result<PreviewEntryIdentity, ActionError> {
    filesystem
        .entry_identity(path)?
        .map(PreviewEntryIdentity::from_cap)
        .ok_or_else(|| {
            ActionError::PreviewStale(format!(
                "{role} disappeared while preparing the reviewed plan: {}",
                path.display()
            ))
        })
}

/// Captures only the album's directory-entry graph. This is sufficient to
/// detect objects appearing or disappearing in ways that could change glob
/// matching without hashing unrelated audio payloads. Concrete operands are
/// content-bound separately by [`preview_operand_graph`].
fn capture_preview_matcher_tree(
    filesystem: &dyn ActionFilesystem,
    context: &ActionContext,
) -> Result<(PreviewEntryIdentity, Vec<PreviewTreeEntry>), ActionError> {
    capture_preview_matcher_tree_cancellable(
        filesystem,
        context,
        &NeverCancel,
        &NoExplicitPreviewProgress,
    )
}

fn capture_preview_matcher_tree_cancellable(
    filesystem: &dyn ActionFilesystem,
    context: &ActionContext,
    cancellation: &dyn ActionCancellation,
    progress: &dyn ExplicitPreviewProgressObserver,
) -> Result<(PreviewEntryIdentity, Vec<PreviewTreeEntry>), ActionError> {
    let subject = preview_entry_identity(filesystem, &context.subject_dir, "album root")?;
    if subject.kind != PreviewEntryKind::Directory {
        return Err(ActionError::UnsafePath(format!(
            "manual action subject is not a directory: {}",
            context.subject_dir.display()
        )));
    }

    let excluded = lexical_normalize(&context.journal_dir);
    let mut tree = Vec::new();
    for (path, _) in filesystem.enumerate_tree_cancellable(
        &context.subject_dir,
        cancellation,
        progress,
    )? {
        if cancellation.is_cancelled() {
            return Err(ActionError::CancelledBeforeMutation(
                "manual action preview preparation was cancelled".to_string(),
            ));
        }
        let normalized = lexical_normalize(&path);
        if normalized == excluded || normalized.starts_with(&excluded) {
            continue;
        }
        let identity = filesystem.entry_identity(&path)?.ok_or_else(|| {
            ActionError::PreviewStale(format!(
                "album entry disappeared while preparing the reviewed plan: {}",
                path.display()
            ))
        })?;
        tree.push(PreviewTreeEntry {
            path: normalized,
            entry_identity: PreviewEntryIdentity::from_cap(identity),
        });
    }
    tree.sort_by(|left, right| left.path.cmp(&right.path));
    Ok((subject, tree))
}

#[derive(Debug)]
struct PreviewRoleInput<'a> {
    path: &'a Path,
    role: &'static str,
    expected_content: Option<&'a ObjectIdentity>,
}

fn preview_operation_roles(operation: &PlannedOperation) -> Vec<PreviewRoleInput<'_>> {
    match operation {
        PlannedOperation::Rename {
            source,
            destination,
            staging,
            expected_source,
            ..
        } => vec![
            PreviewRoleInput {
                path: source,
                role: "rename_source",
                expected_content: Some(expected_source),
            },
            PreviewRoleInput {
                path: destination,
                role: "rename_destination",
                expected_content: None,
            },
            PreviewRoleInput {
                path: staging,
                role: "rename_staging",
                expected_content: None,
            },
        ],
        PlannedOperation::Copy {
            source,
            destination,
            temporary,
            publication_witness,
            expected_source,
        } => vec![
            PreviewRoleInput {
                path: source,
                role: "copy_source",
                expected_content: Some(expected_source),
            },
            PreviewRoleInput {
                path: destination,
                role: "copy_destination",
                expected_content: None,
            },
            PreviewRoleInput {
                path: temporary,
                role: "copy_temporary",
                expected_content: None,
            },
            PreviewRoleInput {
                path: publication_witness,
                role: "copy_publication_witness",
                expected_content: None,
            },
        ],
        PlannedOperation::RepairCopyMetadata {
            source,
            destination,
            expected_source,
            expected_destination,
            ..
        } => vec![
            PreviewRoleInput {
                path: source,
                role: "copy_metadata_source",
                expected_content: Some(expected_source),
            },
            PreviewRoleInput {
                path: destination,
                role: "copy_metadata_destination",
                expected_content: Some(expected_destination),
            },
        ],
        PlannedOperation::Move {
            source,
            destination,
            temporary,
            publication_witness,
            source_witness,
            expected_source,
        } => vec![
            PreviewRoleInput {
                path: source,
                role: "move_source",
                expected_content: Some(expected_source),
            },
            PreviewRoleInput {
                path: destination,
                role: "move_destination",
                expected_content: None,
            },
            PreviewRoleInput {
                path: temporary,
                role: "move_temporary",
                expected_content: None,
            },
            PreviewRoleInput {
                path: publication_witness,
                role: "move_publication_witness",
                expected_content: None,
            },
            PreviewRoleInput {
                path: source_witness,
                role: "move_source_witness",
                expected_content: None,
            },
        ],
        PlannedOperation::Delete {
            target,
            witness,
            expected_target,
        } => vec![
            PreviewRoleInput {
                path: target,
                role: "delete_target",
                expected_content: Some(expected_target),
            },
            PreviewRoleInput {
                path: witness,
                role: "delete_witness",
                expected_content: None,
            },
        ],
        PlannedOperation::CreateDirectory { path } => vec![PreviewRoleInput {
            path,
            role: "create_directory_destination",
            expected_content: None,
        }],
        PlannedOperation::RunScript {
            script,
            expected_script,
            runtime_directory,
            ..
        } => vec![
            PreviewRoleInput {
                path: script,
                role: "runscript_executable",
                expected_content: Some(expected_script),
            },
            PreviewRoleInput {
                path: runtime_directory,
                role: "runscript_runtime",
                expected_content: None,
            },
        ],
    }
}

fn preview_precondition_roles(
    precondition: &PlanningPrecondition,
) -> Vec<PreviewRoleInput<'_>> {
    match precondition {
        PlanningPrecondition::CopyAlreadyEquivalent {
            source,
            destination,
            expected_source,
            expected_destination,
        } => vec![
            PreviewRoleInput {
                path: source,
                role: "copy_noop_source",
                expected_content: Some(expected_source),
            },
            PreviewRoleInput {
                path: destination,
                role: "copy_noop_destination",
                expected_content: Some(expected_destination),
            },
        ],
        PlanningPrecondition::DirectoryAlreadyExists {
            path,
            expected_directory,
        } => vec![PreviewRoleInput {
            path,
            role: "create_directory_noop",
            expected_content: Some(expected_directory),
        }],
        PlanningPrecondition::RenameAlreadyNamed {
            path,
            expected_entry,
        } => vec![PreviewRoleInput {
            path,
            role: "rename_noop_source",
            expected_content: Some(expected_entry),
        }],
        PlanningPrecondition::MoveAlreadyAtDestination {
            path,
            expected_entry,
        } => vec![PreviewRoleInput {
            path,
            role: "move_noop_source",
            expected_content: Some(expected_entry),
        }],
    }
}

#[allow(dead_code)] // bundle-provided API surface, not yet wired to a caller
fn preview_operand_graph(
    filesystem: &dyn ActionFilesystem,
    plans: &[ActionPlan],
) -> Result<PreviewOperandGraph, ActionError> {
    preview_operand_graph_cancellable(filesystem, plans, &NeverCancel)
}

#[allow(dead_code)] // bundle-provided API surface, not yet wired to a caller
fn preview_operand_graph_cancellable(
    filesystem: &dyn ActionFilesystem,
    plans: &[ActionPlan],
    cancellation: &dyn ActionCancellation,
) -> Result<PreviewOperandGraph, ActionError> {
    let binding_total = plans
        .iter()
        .map(|plan| {
            plan.operations.len() as u64 + plan.planning_preconditions.len() as u64
        })
        .sum::<u64>();
    preview_operand_graph_cancellable_observed(
        filesystem,
        plans,
        cancellation,
        &NoExplicitPreviewProgress,
        binding_total,
    )
}

fn preview_operand_graph_cancellable_observed(
    filesystem: &dyn ActionFilesystem,
    plans: &[ActionPlan],
    cancellation: &dyn ActionCancellation,
    progress: &dyn ExplicitPreviewProgressObserver,
    binding_total: u64,
) -> Result<PreviewOperandGraph, ActionError> {
    #[derive(Default)]
    struct ObjectBuilder {
        paths: BTreeMap<PathBuf, BTreeSet<String>>,
        expected_content: Option<ObjectIdentity>,
    }

    let mut objects: BTreeMap<PreviewEntryIdentity, ObjectBuilder> = BTreeMap::new();
    let mut absent: BTreeMap<PathBuf, BTreeSet<String>> = BTreeMap::new();

    let mut completed_bindings = 0_u64;
    for plan in plans {
        for operation in &plan.operations {
            if cancellation.is_cancelled() {
                return Err(ActionError::CancelledBeforeMutation(
                    "manual action preview preparation was cancelled".to_string(),
                ));
            }
            for input in preview_operation_roles(operation) {
                let path = lexical_normalize(input.path);
                match filesystem.entry_identity(input.path)? {
                    Some(identity) => {
                        let entry_identity = PreviewEntryIdentity::from_cap(identity);
                        let builder = objects.entry(entry_identity).or_default();
                        builder
                            .paths
                            .entry(path.clone())
                            .or_default()
                            .insert(input.role.to_string());
                        if let Some(expected) = input.expected_content {
                            let observed = filesystem.identity(input.path, true)?;
                            if !expected.same_object(&observed) {
                                return Err(ActionError::PreviewStale(format!(
                                    "{} changed while preparing the reviewed plan: {}",
                                    input.role,
                                    input.path.display()
                                )));
                            }
                            match builder.expected_content.as_ref() {
                                Some(existing) if !existing.same_object(expected) => {
                                    return Err(ActionError::Contradiction(format!(
                                        "one filesystem object has contradictory reviewed identities: {}",
                                        input.path.display()
                                    )));
                                }
                                Some(_) => {}
                                None => builder.expected_content = Some(expected.clone()),
                            }
                        }
                    }
                    None => {
                        if input.expected_content.is_some() {
                            return Err(ActionError::PreviewStale(format!(
                                "{} disappeared while preparing the reviewed plan: {}",
                                input.role,
                                input.path.display()
                            )));
                        }
                        absent
                            .entry(path)
                            .or_default()
                            .insert(input.role.to_string());
                    }
                }
            }
            completed_bindings += 1;
            progress.update(
                "Binding concrete operand identities",
                completed_bindings,
                Some(binding_total),
            );
        }
        for precondition in &plan.planning_preconditions {
            if cancellation.is_cancelled() {
                return Err(ActionError::CancelledBeforeMutation(
                    "manual action preview preparation was cancelled".to_string(),
                ));
            }
            for input in preview_precondition_roles(precondition) {
                let path = lexical_normalize(input.path);
                let identity = filesystem.entry_identity(input.path)?.ok_or_else(|| {
                    ActionError::PreviewStale(format!(
                        "{} disappeared while preparing the reviewed plan: {}",
                        input.role,
                        input.path.display()
                    ))
                })?;
                let entry_identity = PreviewEntryIdentity::from_cap(identity);
                let builder = objects.entry(entry_identity).or_default();
                builder
                    .paths
                    .entry(path)
                    .or_default()
                    .insert(input.role.to_string());
                let expected = input.expected_content.ok_or_else(|| {
                    ActionError::InvalidJournal(
                        "planning precondition omitted its expected identity".to_string(),
                    )
                })?;
                let observed = filesystem.identity(input.path, true)?;
                if !expected.same_object(&observed) {
                    return Err(ActionError::PreviewStale(format!(
                        "{} changed while preparing the reviewed plan: {}",
                        input.role,
                        input.path.display()
                    )));
                }
                match builder.expected_content.as_ref() {
                    Some(existing) if !existing.same_object(expected) => {
                        return Err(ActionError::Contradiction(format!(
                            "one filesystem object has contradictory reviewed identities: {}",
                            input.path.display()
                        )));
                    }
                    Some(_) => {}
                    None => builder.expected_content = Some(expected.clone()),
                }
            }
            completed_bindings += 1;
            progress.update(
                "Binding planning preconditions",
                completed_bindings,
                Some(binding_total),
            );
        }
    }

    let objects = objects
        .into_iter()
        .map(|(entry_identity, builder)| PreviewObjectExpectation {
            entry_identity,
            paths: builder
                .paths
                .into_iter()
                .map(|(path, roles)| PreviewPathRoles {
                    path,
                    roles: roles.into_iter().collect(),
                })
                .collect(),
            expected_content: builder.expected_content,
        })
        .collect();
    let absent_paths = absent
        .into_iter()
        .map(|(path, roles)| PreviewAbsentExpectation {
            path,
            roles: roles.into_iter().collect(),
        })
        .collect();
    Ok(PreviewOperandGraph {
        objects,
        absent_paths,
    })
}

fn validate_preview_operand_graph(
    filesystem: &dyn ActionFilesystem,
    graph: &PreviewOperandGraph,
) -> Result<(), ActionError> {
    for object in &graph.objects {
        if object.paths.is_empty() {
            return Err(ActionError::InvalidJournal(
                "reviewed operand object has no paths".to_string(),
            ));
        }
        for path_roles in &object.paths {
            let current = filesystem
                .entry_identity(&path_roles.path)?
                .ok_or_else(|| {
                    ActionError::PreviewStale(format!(
                        "reviewed operand disappeared: {}",
                        path_roles.path.display()
                    ))
                })?;
            if !object.entry_identity.matches_cap(current) {
                return Err(ActionError::PreviewStale(format!(
                    "reviewed operand was replaced: {}",
                    path_roles.path.display()
                )));
            }
            if let Some(expected) = object.expected_content.as_ref() {
                let current = filesystem.identity(&path_roles.path, true)?;
                if !expected.same_object(&current) {
                    return Err(ActionError::PreviewStale(format!(
                        "reviewed operand contents changed: {}",
                        path_roles.path.display()
                    )));
                }
            }
        }
    }
    for absent in &graph.absent_paths {
        if filesystem.entry_identity(&absent.path)?.is_some() {
            return Err(ActionError::PreviewStale(format!(
                "a path required to remain absent appeared: {}",
                absent.path.display()
            )));
        }
    }
    Ok(())
}

fn validate_planning_precondition_shapes(
    action: &ConversionAction,
    plan: &ActionPlan,
    context: &ActionContext,
) -> Result<(), ActionError> {
    for precondition in &plan.planning_preconditions {
        match (action, precondition) {
            (ConversionAction::Copy(_), PlanningPrecondition::CopyAlreadyEquivalent { source, destination, .. }) => {
                if !source.starts_with(&context.subject_dir) || source == &context.subject_dir {
                    return Err(ActionError::InvalidJournal(format!(
                        "copy no-op source is outside the action subject: {}",
                        source.display()
                    )));
                }
                reject_protected_action_artifact(source, context, "copy no-op source")?;
                reject_protected_action_artifact(destination, context, "copy no-op destination")?;
            }
            (ConversionAction::CreateFolder(_), PlanningPrecondition::DirectoryAlreadyExists { path, .. }) => {
                reject_protected_action_artifact(path, context, "create_folder no-op destination")?;
            }
            (ConversionAction::Rename(_), PlanningPrecondition::RenameAlreadyNamed { path, .. }) => {
                if !path.starts_with(&context.subject_dir) || path == &context.subject_dir {
                    return Err(ActionError::InvalidJournal(format!(
                        "rename no-op path is outside the action subject: {}",
                        path.display()
                    )));
                }
                reject_protected_action_artifact(path, context, "rename no-op path")?;
            }
            (ConversionAction::Move(_), PlanningPrecondition::MoveAlreadyAtDestination { path, .. }) => {
                if !path.starts_with(&context.subject_dir) || path == &context.subject_dir {
                    return Err(ActionError::InvalidJournal(format!(
                        "move no-op path is outside the action subject: {}",
                        path.display()
                    )));
                }
                reject_protected_action_artifact(path, context, "move no-op path")?;
            }
            _ => {
                return Err(ActionError::InvalidJournal(format!(
                    "{} plan contains a planning precondition for another action kind",
                    plan.action_kind
                )));
            }
        }
    }
    Ok(())
}

fn validate_planning_precondition_identities(
    plan: &ActionPlan,
) -> Result<(), ActionError> {
    for precondition in &plan.planning_preconditions {
        match precondition {
            PlanningPrecondition::CopyAlreadyEquivalent {
                expected_source,
                expected_destination,
                ..
            } => {
                validate_object_identity(expected_source)?;
                validate_object_identity(expected_destination)?;
                if !expected_source.copy_state_equivalent(expected_destination) {
                    return Err(ActionError::InvalidJournal(
                        "copy no-op precondition is not copy-state equivalent".to_string(),
                    ));
                }
            }
            PlanningPrecondition::DirectoryAlreadyExists {
                expected_directory,
                ..
            } => {
                validate_object_identity(expected_directory)?;
                if expected_directory.kind != ObjectKind::Directory {
                    return Err(ActionError::InvalidJournal(
                        "create_folder no-op precondition is not a directory".to_string(),
                    ));
                }
            }
            PlanningPrecondition::RenameAlreadyNamed { expected_entry, .. }
            | PlanningPrecondition::MoveAlreadyAtDestination { expected_entry, .. } => {
                validate_object_identity(expected_entry)?;
            }
        }
    }
    Ok(())
}

fn validate_planning_preconditions(
    filesystem: &dyn ActionFilesystem,
    plans: &[ActionPlan],
) -> Result<(), ActionError> {
    for plan in plans {
        for precondition in &plan.planning_preconditions {
            match precondition {
                PlanningPrecondition::CopyAlreadyEquivalent {
                    source,
                    destination,
                    expected_source,
                    expected_destination,
                } => {
                    let current_source = identity_matching_policy(filesystem, source, expected_source)?;
                    let current_destination =
                        identity_matching_policy(filesystem, destination, expected_destination)?;
                    if !current_source.same_object(expected_source)
                        || !current_source.copy_state_equivalent(expected_source)
                        || !current_destination.same_object(expected_destination)
                        || !current_destination.copy_state_equivalent(expected_destination)
                        || !current_source.copy_state_equivalent(&current_destination)
                    {
                        return Err(ActionError::PreviewStale(format!(
                            "the copy state that justified doing nothing changed: {} -> {}",
                            source.display(),
                            destination.display()
                        )));
                    }
                }
                PlanningPrecondition::DirectoryAlreadyExists {
                    path,
                    expected_directory,
                } => {
                    let current = identity_matching_policy(filesystem, path, expected_directory)?;
                    if current.kind != ObjectKind::Directory
                        || !current.same_object(expected_directory)
                    {
                        return Err(ActionError::PreviewStale(format!(
                            "the existing directory that justified doing nothing changed: {}",
                            path.display()
                        )));
                    }
                }
                PlanningPrecondition::RenameAlreadyNamed { path, expected_entry }
                | PlanningPrecondition::MoveAlreadyAtDestination { path, expected_entry } => {
                    let current = identity_matching_policy(filesystem, path, expected_entry)?;
                    if !current.same_object(expected_entry) {
                        return Err(ActionError::PreviewStale(format!(
                            "the object that justified doing nothing changed: {}",
                            path.display()
                        )));
                    }
                }
            }
        }
    }
    Ok(())
}

fn recovery_operation_previews(journal: &ActionJournal) -> Vec<ExplicitRecoveryOperationPreview> {
    let mut previews = Vec::new();
    for action in &journal.actions {
        for operation in &action.operations {
            let script_started = operation
                .script_execution
                .as_ref()
                .map(|script| script.start_committed || script.user_code_released)
                .unwrap_or(false);
            previews.push(ExplicitRecoveryOperationPreview {
                action_index: action.index,
                action_kind: action
                    .plan
                    .as_ref()
                    .map(|plan| plan.action_kind.clone())
                    .unwrap_or_else(|| "unplanned".to_string()),
                operation_id: operation.operation_id.clone(),
                summary: operation_summary(&operation.plan),
                durable_state: format!("{:?}", operation.state),
                cleanup_only: matches!(
                    operation.state,
                    OperationState::Committed | OperationState::CleanupStarted
                ),
                script_started,
                script_replayable: !script_started,
            });
        }
    }
    previews
}

fn explicit_journal_is_resolved_terminal(
    filesystem: &dyn ActionFilesystem,
    pipeline: &ActionPipeline,
    context: &ActionContext,
) -> Result<bool, ActionError> {
    let retained_live_context = prepare_context_for_journal_read(filesystem, context)?;
    let digest = pipeline.canonical_sha256()?;
    let path = action_journal_path(context, &digest)?;
    let store = JournalStore::new(path.clone(), filesystem)?;
    let loaded = if retained_live_context {
        load_journal_bound(&store)?
    } else {
        load_journal_bootstrap(&store)?
    };
    let Some((journal, _)) = loaded else {
        return Ok(false);
    };
    Ok(validate_resolved_terminal_journal_authority_for_context(
        filesystem,
        &journal,
        &path,
        retained_live_context,
    )
    .is_ok())
}

fn retire_resolved_terminal_journal_locked(
    filesystem: &dyn ActionFilesystem,
    pipeline: &ActionPipeline,
    context: &ActionContext,
) -> Result<(), ActionError> {
    let retained_live_context = prepare_context_for_journal_read(filesystem, context)?;
    let digest = pipeline.canonical_sha256()?;
    let path = action_journal_path(context, &digest)?;
    let store = JournalStore::new(path.clone(), filesystem)?;
    let loaded = if retained_live_context {
        load_journal_bound(&store)?
    } else {
        load_journal_bootstrap(&store)?
    };
    let Some((journal, _)) = loaded else {
        return Ok(());
    };
    validate_resolved_terminal_journal_authority_for_context(
        filesystem,
        &journal,
        &path,
        retained_live_context,
    )?;
    // Retire a write-temporary first. If the process crashes between these
    // removals, the validated terminal journal remains discoverable and can be
    // retired by the next invocation. Removing the final journal first could
    // leave an orphaned temporary that must fail closed as unknown authority.
    if retained_live_context {
        if let Some(identity) = filesystem.entry_identity(&store.temporary)? {
            if identity.file_type != CapFileType::Regular {
                return Err(ActionError::Contradiction(format!(
                    "terminal manual journal temporary is not a regular file: {}",
                    store.temporary.display()
                )));
            }
            filesystem.remove_owned_path(&store.temporary, identity)?;
            filesystem.sync_parent(&store.temporary)?;
        }
        if let Some(identity) = filesystem.entry_identity(&path)? {
            if identity.file_type != CapFileType::Regular {
                return Err(ActionError::Contradiction(format!(
                    "terminal manual journal is not a regular file: {}",
                    path.display()
                )));
            }
            filesystem.remove_owned_path(&path, identity)?;
            filesystem.sync_parent(&path)?;
        }
    } else {
        match fs::remove_file(&store.temporary) {
            Ok(()) => sync_parent(&store.temporary)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        match fs::remove_file(&path) {
            Ok(()) => sync_parent(&path)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn retire_completed_explicit_run_locked(
    filesystem: &dyn ActionFilesystem,
    record: &ExplicitActiveRunRecord,
    context: &ActionContext,
    lock: &ExplicitActionRunLock,
) -> Result<(), ActionError> {
    let pipeline: ActionPipeline = serde_json::from_str(&record.payload.pipeline_serialized)
        .map_err(ActionError::Serialization)?;
    if !explicit_journal_is_resolved_terminal(
        filesystem,
        &pipeline,
        context,
    )? {
        return Err(ActionError::Contradiction(
            "explicit active run cannot be retired without a validated terminal journal"
                .to_string(),
        ));
    }
    // Remove the active pointer first. If the process crashes before journal
    // retirement, the orphan is validated as terminal and discarded before a
    // fresh invocation. The inverse order could replay a completed mutation.
    remove_explicit_active_run_locked(record, context, lock)?;
    retire_resolved_terminal_journal_locked(filesystem, &pipeline, context)
}

fn expected_capability_roots(
    pipeline: &ActionPipeline,
    context: &ActionContext,
    recorded_roots: &[ScopeRecord],
) -> Result<Vec<(ScopeId, PathBuf)>, ActionError> {
    // Mirror prepare_context_capabilities: album batches anchor the source
    // scope at the shared grouping root.
    let source_root = if let Some(root) = context.batch_source_scope_root.clone() {
        root
    } else if context.source_is_directory {
        context.source_path.clone()
    } else {
        context.source_path.parent().ok_or_else(|| {
            ActionError::UnsafePath(format!(
                "source has no parent capability root: {}",
                context.source_path.display()
            ))
        })?.to_path_buf()
    };
    let mut roots = vec![
        (ScopeId::new("subject".to_string())?, context.subject_dir.clone()),
        (ScopeId::new("output".to_string())?, context.output_root.clone()),
        (ScopeId::new("album".to_string())?, context.album_dir.clone()),
        (ScopeId::new("journal".to_string())?, context.journal_dir.clone()),
        (ScopeId::new("source".to_string())?, source_root),
    ];
    for action in pipeline.for_phase(context.phase) {
        let destination_root = match action {
            ConversionAction::Copy(action) => Some(render_action_path(
                &action.destination,
                context,
                &context.subject_dir,
            )?),
            ConversionAction::Move(action) => Some(render_action_path(
                &action.destination,
                context,
                &context.subject_dir,
            )?),
            ConversionAction::CreateFolder(action) => {
                let path = render_action_path(&action.path, context, &context.subject_dir)?;
                Some(path.parent().ok_or_else(|| {
                    ActionError::UnsafePath(format!(
                        "create_folder target has no parent: {}",
                        path.display()
                    ))
                })?.to_path_buf())
            }
            ConversionAction::Runscript(action) => {
                let script = resolve_script_path(&action.script)?;
                Some(script.parent().ok_or_else(|| {
                    ActionError::UnsafePath(format!(
                        "runscript path has no parent capability root: {}",
                        script.display()
                    ))
                })?.to_path_buf())
            }
            ConversionAction::Rename(_) | ConversionAction::Delete(_) => None,
        };
        if let Some(root) = destination_root {
            let (scope_prefix, root) = match action {
                ConversionAction::Runscript(_) => {
                    (format!("script-{}", context.phase.as_str()), root)
                }
                ConversionAction::Copy(_)
                | ConversionAction::Move(_)
                | ConversionAction::CreateFolder(_) => {
                    let stable =
                        destination_materialization_authority_root(context, &root)?;
                    let root = if stable != root {
                        stable
                    } else {
                        recorded_destination_authority_root(
                            recorded_roots,
                            context.phase,
                            &root,
                        )?
                        .unwrap_or(root)
                    };
                    (format!("destination-{}", context.phase.as_str()), root)
                }
                ConversionAction::Rename(_) | ConversionAction::Delete(_) => unreachable!(),
            };
            roots.push((deterministic_scope_id(&scope_prefix, &root)?, root));
        }
    }
    roots.sort_by(|left, right| left.0.cmp(&right.0));
    roots.dedup_by(|left, right| left.0 == right.0 && left.1 == right.1);
    Ok(roots)
}

fn recorded_destination_authority_root(
    records: &[ScopeRecord],
    phase: ActionPhase,
    destination_root: &Path,
) -> Result<Option<PathBuf>, ActionError> {
    let prefix = format!("destination-{}", phase.as_str());
    let id_prefix = format!("{prefix}-");
    let mut selected: Option<&ScopeRecord> = None;
    for record in records.iter().filter(|record| {
        record.id.as_str().starts_with(&id_prefix)
            && destination_root.starts_with(&record.logical_path)
    }) {
        let expected_id = deterministic_scope_id(&prefix, &record.logical_path)?;
        if record.id != expected_id {
            return Err(ActionError::InvalidJournal(format!(
                "destination capability scope {} does not match its logical root {}",
                record.id.as_str(),
                record.logical_path.display()
            )));
        }
        if record.logical_path.as_path() != destination_root {
            if record.materialization_token.is_none() {
                // An existing exact destination from another action is not a
                // shared first-publication authority. Let this destination
                // acquire its own narrower root beneath that existing parent.
                continue;
            }
            let mut components = record.base_relative.as_path().components();
            let exactly_one_component = matches!(components.next(), Some(Component::Normal(_)))
                && components.next().is_none();
            let expected_logical_path = lexical_normalize_absolute(
                &record
                    .acquisition_path
                    .join(record.base_relative.as_path()),
            )?;
            if !exactly_one_component
                || expected_logical_path.as_path() != record.logical_path.as_path()
            {
                return Err(ActionError::InvalidJournal(format!(
                    "shared external destination scope {} is not the first missing component beneath its recorded acquisition root",
                    record.logical_path.display()
                )));
            }
        }
        let replace = selected
            .map(|current| {
                record.logical_path.components().count()
                    > current.logical_path.components().count()
            })
            .unwrap_or(true);
        if replace {
            selected = Some(record);
        }
    }
    Ok(selected.map(|record| record.logical_path.clone()))
}

fn prepare_context_capabilities(
    filesystem: &dyn ActionFilesystem,
    context: &ActionContext,
) -> Result<(), ActionError> {
    for (label, path) in [
        ("subject", context.subject_dir.as_path()),
        ("source", context.source_path.as_path()),
        ("output", context.output_root.as_path()),
        ("album", context.album_dir.as_path()),
        ("journal", context.journal_dir.as_path()),
    ] {
        reject_ephemeral_descriptor_namespace_path(label, path)?;
    }
    // Album batches pin the source scope at the shared grouping root so every
    // participant registers the identical scope identity (see
    // `batch_source_scope_root` on ActionContext).
    let source_root = if let Some(root) = context.batch_source_scope_root.as_deref() {
        root
    } else if context.source_is_directory {
        context.source_path.as_path()
    } else {
        context.source_path.parent().ok_or_else(|| {
            ActionError::UnsafePath(format!(
                "source has no parent capability root: {}",
                context.source_path.display()
            ))
        })?
    };
    for (name, path) in [
        ("subject", context.subject_dir.as_path()),
        ("output", context.output_root.as_path()),
        ("album", context.album_dir.as_path()),
        ("journal", context.journal_dir.as_path()),
        ("source", source_root),
    ] {
        let id = ScopeId::new(name.to_string())?;
        let retained = match name {
            "subject" | "album" if path == context.album_dir => {
                context.retained_album_capability.as_deref()
            }
            "source" if path == context.album_dir => {
                context.retained_album_capability.as_deref()
            }
            "output" => context.retained_output_capability.as_deref(),
            "journal" => context.retained_journal_capability.as_deref(),
            _ => None,
        };
        if let Some(capability) = retained {
            if name == "journal" {
                filesystem.pin_existing_recoverable_capability(id, path, capability)?;
            } else {
                filesystem.pin_existing_capability(id, path, capability)?;
            }
        } else if name == "journal" {
            filesystem.pin_recoverable_internal_root(id, path)?;
        } else if name == "album" && context.phase == ActionPhase::Pre {
            filesystem.pin_materializable_root(id, path)?;
        } else {
            filesystem.pin_root(id, path)?;
        }
    }
    Ok(())
}

fn reject_ephemeral_descriptor_namespace_path(
    label: &str,
    path: &Path,
) -> Result<(), ActionError> {
    let normalized = path.to_string_lossy().replace('\\', "/");
    let ephemeral = normalized == "/proc/self/fd"
        || normalized.starts_with("/proc/self/fd/")
        || normalized == "/dev/fd"
        || normalized.starts_with("/dev/fd/");
    if ephemeral {
        return Err(ActionError::UnsafePath(format!(
            "{label} durable identity must not use ephemeral descriptor namespace path {}",
            path.display()
        )));
    }
    Ok(())
}

fn retained_ancestor_for_destination<'a>(
    context: &'a ActionContext,
    destination_root: &Path,
) -> Option<(&'a Path, &'a PinnedDirectoryCapability)> {
    let candidates = [
        context
            .retained_journal_capability
            .as_deref()
            .map(|capability| (context.journal_dir.as_path(), capability)),
        context
            .retained_album_capability
            .as_deref()
            .map(|capability| (context.album_dir.as_path(), capability)),
        context
            .retained_output_capability
            .as_deref()
            .map(|capability| (context.output_root.as_path(), capability)),
    ];
    candidates
        .into_iter()
        .flatten()
        .filter(|(logical_root, _)| destination_root.starts_with(logical_root))
        .max_by_key(|(logical_root, _)| logical_root.components().count())
}

/// Return the durable materialization boundary for a rendered mutation root.
///
/// A destination nested beneath a stable action-context root is intentionally
/// scoped at the first component below the most-specific such root.  This
/// makes sibling destinations such as `exports/logs` and `exports/cues` share
/// one first-publication token for `exports`, rather than assigning two tokens
/// that can never both authenticate the same directory entry.  Paths beneath
/// the shared boundary remain individually journaled as operation operands.
///
/// The selection is purely lexical over stable logical paths, so restart
/// recovery derives the same capability-root set after the shared prefix has
/// materialized. Destinations outside every stable context root are handled
/// by a no-follow nearest-existing-ancestor probe and then recovered from the
/// durable scope record, rather than recomputing from changed path existence.
fn destination_stable_anchor<'a>(
    context: &'a ActionContext,
    destination_root: &Path,
) -> Option<&'a Path> {
    let stable_roots = [
        context.journal_dir.as_path(),
        context.album_dir.as_path(),
        context.subject_dir.as_path(),
        context.output_root.as_path(),
    ];
    stable_roots
        .into_iter()
        .filter(|root| destination_root.starts_with(root))
        .max_by_key(|root| root.components().count())
}

fn destination_materialization_authority_root(
    context: &ActionContext,
    destination_root: &Path,
) -> Result<PathBuf, ActionError> {
    let destination_root = lexical_normalize_absolute(destination_root)?;
    let Some(anchor) = destination_stable_anchor(context, &destination_root) else {
        return Ok(destination_root);
    };
    let relative = destination_root.strip_prefix(anchor).map_err(|_| {
        ActionError::UnsafePath(format!(
            "destination root escaped stable capability anchor: {}",
            destination_root.display()
        ))
    })?;
    let mut components = relative.components();
    let Some(first) = components.next() else {
        return Ok(anchor.to_path_buf());
    };
    let Component::Normal(first) = first else {
        return Err(ActionError::UnsafePath(format!(
            "destination root contains an unstable component: {}",
            destination_root.display()
        )));
    };
    Ok(anchor.join(first))
}

fn pin_exact_rendered_root(
    filesystem: &dyn ActionFilesystem,
    scope_prefix: &str,
    context: &ActionContext,
    root: &Path,
) -> Result<(), ActionError> {
    let id = deterministic_scope_id(scope_prefix, root)?;
    if let Some((ancestor_logical_path, capability)) =
        retained_ancestor_for_destination(context, root)
    {
        filesystem.pin_descendant_capability(id, root, ancestor_logical_path, capability)
    } else {
        filesystem.pin_root(id, root)
    }
}

fn pin_rendered_destination_root(
    filesystem: &dyn ActionFilesystem,
    context: &ActionContext,
    _action_index: usize,
    destination_root: &Path,
) -> Result<(), ActionError> {
    let destination_root = lexical_normalize_absolute(destination_root)?;
    let stable_anchor = destination_stable_anchor(context, &destination_root).is_some();
    let authority_root = if stable_anchor {
        destination_materialization_authority_root(context, &destination_root)?
    } else if let Some(existing) = recorded_destination_authority_root(
        &filesystem.scope_records()?,
        context.phase,
        &destination_root,
    )? {
        existing
    } else {
        filesystem.first_materialization_boundary(&destination_root)?
    };
    let prefix = format!("destination-{}", context.phase.as_str());
    pin_exact_rendered_root(filesystem, &prefix, context, &authority_root)?;
    if !stable_anchor && authority_root != destination_root {
        let records = filesystem.scope_records()?;
        let observed = recorded_destination_authority_root(
            &records,
            context.phase,
            &destination_root,
        )?;
        if observed.as_deref() != Some(authority_root.as_path()) {
            return Err(ActionError::Contradiction(format!(
                "external destination {} did not retain its probed shared materialization authority {}",
                destination_root.display(),
                authority_root.display()
            )));
        }
    }
    Ok(())
}

fn prepare_retained_pipeline_capabilities(
    filesystem: &dyn ActionFilesystem,
    pipeline: &ActionPipeline,
    context: &ActionContext,
) -> Result<(), ActionError> {
    for action in pipeline.for_phase(context.phase) {
        let destination_root = match action {
            ConversionAction::Copy(action) => Some(render_action_path(
                &action.destination,
                context,
                &context.subject_dir,
            )?),
            ConversionAction::Move(action) => Some(render_action_path(
                &action.destination,
                context,
                &context.subject_dir,
            )?),
            ConversionAction::CreateFolder(action) => {
                let path = render_action_path(&action.path, context, &context.subject_dir)?;
                Some(path.parent().ok_or_else(|| {
                    ActionError::UnsafePath(format!(
                        "create_folder target has no parent: {}",
                        path.display()
                    ))
                })?.to_path_buf())
            }
            ConversionAction::Runscript(action) => {
                let script = resolve_script_path(&action.script)?;
                Some(script.parent().ok_or_else(|| {
                    ActionError::UnsafePath(format!(
                        "runscript path has no parent capability root: {}",
                        script.display()
                    ))
                })?.to_path_buf())
            }
            ConversionAction::Rename(_) | ConversionAction::Delete(_) => None,
        };
        let Some(destination_root) = destination_root else {
            continue;
        };
        let (scope_prefix, destination_root) = match action {
            ConversionAction::Runscript(_) => (
                format!("script-{}", context.phase.as_str()),
                destination_root,
            ),
            ConversionAction::Copy(_)
            | ConversionAction::Move(_)
            | ConversionAction::CreateFolder(_) => {
                (
                    format!("destination-{}", context.phase.as_str()),
                    destination_materialization_authority_root(context, &destination_root)?,
                )
            }
            ConversionAction::Rename(_) | ConversionAction::Delete(_) => unreachable!(),
        };
        let Some((ancestor_logical_path, capability)) =
            retained_ancestor_for_destination(context, &destination_root)
        else {
            continue;
        };
        // During journal recovery, descendant destination scopes must be
        // reconstructed from their durable acquisition/base-relative record.
        // Prebinding a now-materialized descendant as an exact root would
        // erase that durable first-materialization shape. Exact retained roots
        // can be installed immediately; descendants are restored through the
        // already-pinned retained ancestor in `restore_scope_records`.
        if destination_root.as_path() != ancestor_logical_path {
            continue;
        }
        filesystem.pin_existing_capability(
            deterministic_scope_id(&scope_prefix, &destination_root)?,
            &destination_root,
            capability,
        )?;
    }
    Ok(())
}

fn prepare_pipeline_capabilities(
    filesystem: &dyn ActionFilesystem,
    pipeline: &ActionPipeline,
    context: &ActionContext,
) -> Result<(), ActionError> {
    for (action_index, action) in pipeline.for_phase(context.phase).iter().enumerate() {
        match action {
            ConversionAction::Copy(action) => {
                let root = render_action_path(&action.destination, context, &context.subject_dir)?;
                pin_rendered_destination_root(filesystem, context, action_index, &root)?;
            }
            ConversionAction::Move(action) => {
                let root = render_action_path(&action.destination, context, &context.subject_dir)?;
                pin_rendered_destination_root(filesystem, context, action_index, &root)?;
            }
            ConversionAction::CreateFolder(action) => {
                let path = render_action_path(&action.path, context, &context.subject_dir)?;
                let root = path.parent().ok_or_else(|| {
                    ActionError::UnsafePath(format!(
                        "create_folder target has no parent: {}",
                        path.display()
                    ))
                })?;
                pin_rendered_destination_root(filesystem, context, action_index, root)?;
            }
            ConversionAction::Runscript(action) => {
                let script = resolve_script_path(&action.script)?;
                let root = script.parent().ok_or_else(|| {
                    ActionError::UnsafePath(format!(
                        "runscript path has no parent capability root: {}",
                        script.display()
                    ))
                })?;
                let prefix = format!("script-{}", context.phase.as_str());
                pin_exact_rendered_root(filesystem, &prefix, context, root)?;
            }
            ConversionAction::Rename(_) | ConversionAction::Delete(_) => {}
        }
    }
    Ok(())
}

fn validate_phase_action(
    action: &ConversionAction,
    context: &ActionContext,
) -> Result<(), ActionError> {
    if context.phase == ActionPhase::Pre {
        match action {
            ConversionAction::Runscript(_) => return Ok(()),
            ConversionAction::CreateFolder(_) if context.source_is_directory => return Ok(()),
            ConversionAction::CreateFolder(_) => {
                return Err(ActionError::Conflict(
                    "pre create_folder is unavailable for archive/image sources; only runscript is allowed"
                        .to_string(),
                ));
            }
            _ => {
                return Err(ActionError::Conflict(
                    "pre phase permits only runscript and, for directory sources, create_folder"
                        .to_string(),
                ));
            }
        }
    }
    Ok(())
}

fn plan_rename(
    action_index: usize,
    action: &RenameAction,
    context: &ActionContext,
    claim_id: &str,
    filesystem: &dyn ActionFilesystem,
) -> Result<ActionPlan, ActionError> {
    if action.mode == RenameMode::Template && action.template.trim().is_empty() {
        return Err(ActionError::Conflict(
            "template rename requires a non-empty template".to_string(),
        ));
    }
    let matches = collect_targets(&action.targeting, context, filesystem)?;
    let mut intents = Vec::new();
    let mut identities = BTreeMap::new();
    let mut planning_preconditions = Vec::new();
    let mut notices = Vec::new();
    for matched in matches {
        reject_protected_action_artifact(&matched.path, context, "rename source")?;
        if let Err(error) = reject_protected_source(&matched.path, &action.targeting, context) {
            notices.push(error.to_string());
            continue;
        }
        reject_wildcard_directory_with_hidden_descendants(filesystem, &matched)?;
        let identity = filesystem.identity(&matched.path, matched.exact_target)?;
        let destination = rename_destination_for_kind(
            action,
            context,
            &matched.path,
            identity.kind == ObjectKind::Directory,
        )?;
        reject_protected_action_artifact(&destination, context, "rename destination")?;
        if destination == matched.path {
            planning_preconditions.push(PlanningPrecondition::RenameAlreadyNamed {
                path: matched.path.clone(),
                expected_entry: identity,
            });
            notices.push(format!(
                "already named correctly: {}",
                matched.path.display()
            ));
            continue;
        }
        identities.insert(matched.path.clone(), identity);
        intents.push(RenameIntent {
            source: matched.path,
            destination,
        });
    }
    let transaction = plan_rename_transaction(&context.subject_dir, intents)
        .map_err(ActionError::Conflict)?;
    let staging_root = rename_staging_root(context, claim_id, action_index)?;
    let staging_order = transaction.staging_order();
    let mut stage_rank = BTreeMap::new();
    for (rank, index) in staging_order.into_iter().enumerate() {
        stage_rank.insert(index, rank);
    }
    let mut operations = Vec::new();
    for (index, entry) in transaction.entries.iter().enumerate() {
        let rank = stage_rank.get(&index).copied().unwrap_or(index);
        let staging = staging_root.join(format!("{rank:06}"));
        let expected_source = identities
            .remove(&entry.source)
            .ok_or_else(|| ActionError::InvalidJournal("rename identity missing".to_string()))?;
        let excluded_descendants = transaction
            .entries
            .iter()
            .filter(|candidate| {
                candidate.source != entry.source && candidate.source.starts_with(&entry.source)
            })
            .map(|candidate| candidate.source.clone())
            .collect::<Vec<_>>();
        let expected_staged = if excluded_descendants.is_empty() {
            expected_source.clone()
        } else {
            filesystem.identity_excluding(
                &entry.source,
                true,
                &excluded_descendants,
            )?
        };
        operations.push(PlannedOperation::Rename {
            source: entry.source.clone(),
            destination: entry.destination.clone(),
            staging,
            expected_source,
            expected_staged,
        });
    }
    // Preserve shared transaction installation order in the serialized plan by
    // sorting only the eventual publication rank into destination paths. The
    // executor independently recomputes and validates both orders.
    validate_rename_operation_map(&operations, &transaction)?;
    Ok(ActionPlan {
        action_kind: "rename".to_string(),
        operations,
        planning_preconditions,
        notices,
    })
}

fn validate_rename_operation_map(
    operations: &[PlannedOperation],
    transaction: &RenameTransactionPlan,
) -> Result<(), ActionError> {
    if operations.len() != transaction.entries.len() {
        return Err(ActionError::InvalidJournal(
            "rename transaction/operation cardinality mismatch".to_string(),
        ));
    }
    Ok(())
}

fn rename_destination_for_kind(
    action: &RenameAction,
    context: &ActionContext,
    source: &Path,
    is_directory: bool,
) -> Result<PathBuf, ActionError> {
    let file_name = source.file_name().ok_or_else(|| {
        ActionError::UnsafePath(format!("rename source has no file name: {}", source.display()))
    })?;
    let stem = if is_directory {
        file_name.to_string_lossy().to_string()
    } else {
        source
            .file_stem()
            .map(|value| value.to_string_lossy().to_string())
            .unwrap_or_default()
    };
    let rendered = match action.mode {
        RenameMode::Template => (context.semantics.render_template)(
            &action.template,
            &context.tokens_for_path(source),
        )
        .map_err(ActionError::Conflict)?,
        RenameMode::Uppercase => stem.to_uppercase(),
        RenameMode::Lowercase => stem.to_lowercase(),
        RenameMode::Fixcaps => (context.semantics.fixcaps)(&stem),
    };
    let sanitized = (context.semantics.sanitize_component)(&rendered);
    validate_single_component(&sanitized)?;
    let mut destination_name = OsString::from(sanitized);
    if !is_directory {
        if let Some(extension) = source.extension() {
            destination_name.push(".");
            destination_name.push(extension);
        }
    }
    Ok(source.with_file_name(destination_name))
}

fn plan_copy(
    action_index: usize,
    action: &CopyAction,
    context: &ActionContext,
    claim_id: &str,
    filesystem: &dyn ActionFilesystem,
) -> Result<ActionPlan, ActionError> {
    let destination_root = render_action_path(&action.destination, context, &context.subject_dir)?;
    validate_mutation_path(&destination_root, false)?;
    reject_protected_action_artifact(&destination_root, context, "copy destination root")?;
    pin_rendered_destination_root(filesystem, context, action_index, &destination_root)?;
    let mut operations = Vec::new();
    let mut planning_preconditions = Vec::new();
    let mut notices = Vec::new();
    let mut planned_destinations = BTreeMap::<String, PathBuf>::new();
    let matches = collapse_descendant_targets(collect_targets(&action.targeting, context, filesystem)?);
    for (operation_index, matched) in matches.into_iter().enumerate() {
        reject_protected_action_artifact(&matched.path, context, "copy source")?;
        reject_wildcard_directory_with_hidden_descendants(filesystem, &matched)?;
        let file_name = matched.path.file_name().ok_or_else(|| {
            ActionError::UnsafePath(format!("copy source has no file name: {}", matched.path.display()))
        })?;
        let destination = destination_root.join(file_name);
        reject_protected_action_artifact(&destination, context, "copy destination")?;
        if destination != matched.path && destination.starts_with(&matched.path) {
            return Err(ActionError::Conflict(format!(
                "copy destination may not be nested inside its source: {} -> {}",
                matched.path.display(),
                destination.display()
            )));
        }
        register_planned_destination(
            &mut planned_destinations,
            &destination,
            "copy",
        )?;
        let expected_source = filesystem.identity(&matched.path, matched.exact_target)?;
        if filesystem.path_exists_no_follow(&destination)? {
            let existing = filesystem.identity(&destination, matched.exact_target)?;
            if existing.copy_state_equivalent(&expected_source) {
                planning_preconditions.push(PlanningPrecondition::CopyAlreadyEquivalent {
                    source: matched.path.clone(),
                    destination: destination.clone(),
                    expected_source,
                    expected_destination: existing,
                });
                notices.push(format!("already copied: {}", destination.display()));
                continue;
            }
            if existing.same_content(&expected_source) {
                operations.push(PlannedOperation::RepairCopyMetadata {
                    source: matched.path,
                    destination,
                    expected_source,
                    expected_destination: existing,
                    include_hidden: matched.exact_target,
                });
                continue;
            }
            return Err(ActionError::Conflict(format!(
                "copy destination exists with different content: {}",
                destination.display()
            )));
        }
        let temporary = action_temporary_path(
            &destination,
            claim_id,
            action_index,
            operation_index,
            "copy",
        )?;
        let publication_witness = publication_witness_path(
            &destination,
            claim_id,
            action_index,
            operation_index,
            expected_source.kind,
        )?;
        operations.push(PlannedOperation::Copy {
            source: matched.path,
            destination,
            temporary,
            publication_witness,
            expected_source,
        });
    }
    Ok(ActionPlan {
        action_kind: "copy".to_string(),
        operations,
        planning_preconditions,
        notices,
    })
}

fn plan_move(
    action_index: usize,
    action: &MoveAction,
    context: &ActionContext,
    claim_id: &str,
    filesystem: &dyn ActionFilesystem,
) -> Result<ActionPlan, ActionError> {
    let destination_root = render_action_path(&action.destination, context, &context.subject_dir)?;
    validate_mutation_path(&destination_root, false)?;
    reject_protected_action_artifact(&destination_root, context, "move destination root")?;
    pin_rendered_destination_root(filesystem, context, action_index, &destination_root)?;
    let mut operations = Vec::new();
    let mut planning_preconditions = Vec::new();
    let mut notices = Vec::new();
    let mut planned_destinations = BTreeMap::<String, PathBuf>::new();
    let matches = collapse_descendant_targets(collect_targets(&action.targeting, context, filesystem)?);
    for (operation_index, matched) in matches.into_iter().enumerate() {
        reject_protected_action_artifact(&matched.path, context, "move source")?;
        if let Err(error) = reject_protected_source(&matched.path, &action.targeting, context) {
            notices.push(error.to_string());
            continue;
        }
        reject_wildcard_directory_with_hidden_descendants(filesystem, &matched)?;
        let file_name = matched.path.file_name().ok_or_else(|| {
            ActionError::UnsafePath(format!("move source has no file name: {}", matched.path.display()))
        })?;
        let destination = destination_root.join(file_name);
        reject_protected_action_artifact(&destination, context, "move destination")?;
        if destination == matched.path {
            planning_preconditions.push(PlanningPrecondition::MoveAlreadyAtDestination {
                path: matched.path.clone(),
                expected_entry: filesystem.identity(&matched.path, matched.exact_target)?,
            });
            notices.push(format!("already at move destination: {}", destination.display()));
            continue;
        }
        if destination.starts_with(&matched.path) {
            return Err(ActionError::Conflict(format!(
                "move destination may not be nested inside its source: {} -> {}",
                matched.path.display(),
                destination.display()
            )));
        }
        register_planned_destination(
            &mut planned_destinations,
            &destination,
            "move",
        )?;
        if filesystem.path_exists_no_follow(&destination)? {
            return Err(ActionError::Conflict(format!(
                "move destination already exists: {}",
                destination.display()
            )));
        }
        let expected_source = filesystem.identity(&matched.path, matched.exact_target)?;
        let temporary = action_temporary_path(
            &destination,
            claim_id,
            action_index,
            operation_index,
            "move-copy",
        )?;
        let publication_witness = publication_witness_path(
            &destination,
            claim_id,
            action_index,
            operation_index,
            expected_source.kind,
        )?;
        let source_witness = same_directory_witness(
            &matched.path,
            claim_id,
            action_index,
            operation_index,
            "move-source",
        )?;
        operations.push(PlannedOperation::Move {
            source: matched.path,
            destination,
            temporary,
            publication_witness,
            source_witness,
            expected_source,
        });
    }
    Ok(ActionPlan {
        action_kind: "move".to_string(),
        operations,
        planning_preconditions,
        notices,
    })
}

/// Conservative collision namespace for action destinations.
///
/// This deliberately differs from conversion filesystem identity. Action plans
/// must remain safe when their destination lives on a case-insensitive volume,
/// including such volumes mounted from an otherwise case-sensitive host. Rust
/// exposes no portable, side-effect-free query for prospective path case
/// sensitivity, so rejecting case-only destination variants is the safer action
/// transaction contract. Conversion batching must not reuse this key.
fn portable_destination_collision_key(path: &Path) -> String {
    path.to_string_lossy().to_lowercase()
}

fn register_planned_destination(
    planned: &mut BTreeMap<String, PathBuf>,
    destination: &Path,
    action_kind: &str,
) -> Result<(), ActionError> {
    let key = portable_destination_collision_key(destination);
    if let Some(previous) = planned.insert(key, destination.to_path_buf()) {
        return Err(ActionError::Conflict(format!(
            "{action_kind} end-state collision: {} and {} resolve to the same destination",
            previous.display(),
            destination.display()
        )));
    }
    Ok(())
}

fn plan_delete(
    action_index: usize,
    action: &DeleteAction,
    context: &ActionContext,
    claim_id: &str,
    filesystem: &dyn ActionFilesystem,
) -> Result<ActionPlan, ActionError> {
    let mut operations = Vec::new();
    let mut notices = Vec::new();
    let mut matches = collapse_descendant_targets(collect_targets(&action.targeting, context, filesystem)?);
    matches.sort_by(|left, right| {
        right
            .path
            .components()
            .count()
            .cmp(&left.path.components().count())
            .then_with(|| left.path.cmp(&right.path))
    });
    for (operation_index, matched) in matches.into_iter().enumerate() {
        reject_protected_action_artifact(&matched.path, context, "delete target")?;
        if let Err(error) = reject_protected_source(&matched.path, &action.targeting, context) {
            notices.push(error.to_string());
            continue;
        }
        reject_wildcard_directory_with_hidden_descendants(filesystem, &matched)?;
        let expected_target = filesystem.identity(&matched.path, matched.exact_target)?;
        let witness = same_directory_witness(
            &matched.path,
            claim_id,
            action_index,
            operation_index,
            "delete",
        )?;
        operations.push(PlannedOperation::Delete {
            target: matched.path,
            witness,
            expected_target,
        });
    }
    Ok(ActionPlan {
        action_kind: "delete".to_string(),
        operations,
        planning_preconditions: Vec::new(),
        notices,
    })
}

fn plan_create_folder(
    action_index: usize,
    action: &CreateFolderAction,
    context: &ActionContext,
    filesystem: &dyn ActionFilesystem,
) -> Result<ActionPlan, ActionError> {
    let path = render_action_path(&action.path, context, &context.subject_dir)?;
    validate_mutation_path(&path, false)?;
    reject_protected_action_artifact(&path, context, "create_folder destination")?;
    let root = path.parent().ok_or_else(|| {
        ActionError::UnsafePath(format!("create_folder target has no parent: {}", path.display()))
    })?;
    pin_rendered_destination_root(filesystem, context, action_index, root)?;
    if filesystem.path_exists_no_follow(&path)? {
        let identity = filesystem.identity(&path, true)?;
        if identity.kind == ObjectKind::Directory {
            return Ok(ActionPlan {
                action_kind: "create_folder".to_string(),
                operations: Vec::new(),
                planning_preconditions: vec![PlanningPrecondition::DirectoryAlreadyExists {
                    path: path.clone(),
                    expected_directory: identity,
                }],
                notices: vec![format!("folder already exists: {}", path.display())],
            });
        }
        return Err(ActionError::Conflict(format!(
            "create_folder target exists and is not a directory: {}",
            path.display()
        )));
    }
    Ok(ActionPlan {
        action_kind: "create_folder".to_string(),
        operations: vec![PlannedOperation::CreateDirectory { path }],
        planning_preconditions: Vec::new(),
        notices: Vec::new(),
    })
}

fn plan_script(
    action_index: usize,
    action: &RunScriptAction,
    context: &ActionContext,
    claim_id: &str,
    filesystem: &dyn ActionFilesystem,
) -> Result<ActionPlan, ActionError> {
    if action.timeout_seconds == 0 || action.timeout_seconds > MAX_SCRIPT_TIMEOUT_SECONDS {
        return Err(ActionError::Conflict(format!(
            "script timeout must be between 1 and {MAX_SCRIPT_TIMEOUT_SECONDS} seconds"
        )));
    }
    let script = resolve_script_path(&action.script)?;
    validate_executable_script(&script)?;
    let expected_script = filesystem.identity(&script, true)?;
    if expected_script.kind != ObjectKind::File {
        return Err(ActionError::Script(format!(
            "script must resolve to a regular file: {}",
            script.display()
        )));
    }
    let environment = build_script_environment(context);
    let runtime_directory = script_runtime_directory(context, claim_id, action_index, 0)?;
    let containment_token = script_containment_token(claim_id, action_index, 0);
    Ok(ActionPlan {
        action_kind: "runscript".to_string(),
        operations: vec![PlannedOperation::RunScript {
            script,
            expected_script,
            args: action.args.clone(),
            working_directory: context.subject_dir.clone(),
            environment,
            timeout_seconds: action.timeout_seconds,
            runtime_directory,
            containment_token,
        }],
        planning_preconditions: Vec::new(),
        notices: Vec::new(),
    })
}


/// Compare a journal-recorded script environment against the validating
/// context. The elected writer's own validation is exact. A batch member
/// reading the album-scoped journal recomputes every key, but per-request
/// album-token VALUES (TONEPOET_<token>) legitimately differ between the
/// elected track and the reader's track; key set and every other value stay
/// strict.
fn script_environment_matches(
    recorded: &BTreeMap<String, String>,
    context: &ActionContext,
    exempt_request_token_values: bool,
) -> bool {
    let expected = build_script_environment(context);
    if !exempt_request_token_values {
        return recorded == &expected;
    }
    if recorded.len() != expected.len() {
        return false;
    }
    let token_keys: BTreeSet<String> = context
        .album_tokens
        .keys()
        .map(|token| format!("TONEPOET_{}", token.to_ascii_uppercase()))
        .filter(|key| valid_environment_key(key))
        .collect();
    recorded.iter().all(|(key, value)| match expected.get(key) {
        None => false,
        Some(expected_value) => value == expected_value || token_keys.contains(key),
    })
}

fn build_script_environment(context: &ActionContext) -> BTreeMap<String, String> {
    let mut environment = BTreeMap::new();
    environment.insert(
        "PATH".to_string(),
        "/usr/local/bin:/usr/bin:/bin".to_string(),
    );
    environment.insert("LANG".to_string(), "C.UTF-8".to_string());
    if let Some(home) = dirs::home_dir() {
        environment.insert("HOME".to_string(), sanitize_env_value(&home.to_string_lossy()));
    }
    environment.insert("TONEPOET_PHASE".to_string(), context.phase.as_str().to_string());
    environment.insert(
        "TONEPOET_ALBUM_DIR".to_string(),
        sanitize_env_value(
            &context
                .environment_album_dir
                .as_deref()
                .unwrap_or(&context.album_dir)
                .to_string_lossy(),
        ),
    );
    environment.insert(
        "TONEPOET_SOURCE_PATH".to_string(),
        sanitize_env_value(&context.source_path.to_string_lossy()),
    );
    environment.insert(
        "TONEPOET_OUTPUT_ROOT".to_string(),
        sanitize_env_value(&context.output_root.to_string_lossy()),
    );
    for (token, value) in &context.album_tokens {
        let key = format!("TONEPOET_{}", token.to_ascii_uppercase());
        if valid_environment_key(&key) {
            environment.insert(key, sanitize_env_value(value));
        }
    }
    environment.insert(
        "TONEPOET_DISC_COUNT".to_string(),
        context
            .disc_count
            .map(|count| count.to_string())
            .unwrap_or_default(),
    );
    environment
}

#[derive(Debug, Clone)]
struct MatchedTarget {
    path: PathBuf,
    exact_target: bool,
}

fn collect_targets(
    spec: &TargetSpec,
    context: &ActionContext,
    filesystem: &dyn ActionFilesystem,
) -> Result<Vec<MatchedTarget>, ActionError> {
    validate_target_patterns(spec)?;
    if spec.target.is_empty() {
        return Err(ActionError::Conflict(
            "targeting action requires at least one include pattern".to_string(),
        ));
    }
    let subject = context.subject_dir.clone();
    let mut results = BTreeMap::<PathBuf, bool>::new();
    let exact_patterns: Vec<(String, PathBuf)> = spec
        .target
        .iter()
        .filter(|pattern| !contains_wildcard(pattern))
        .map(|pattern| {
            let relative = checked_relative_target(pattern)?;
            Ok((pattern.clone(), subject.join(relative)))
        })
        .collect::<Result<_, ActionError>>()?;

    for (_, path) in &exact_patterns {
        if path_intersects_reserved_action_authority(path, context)? {
            return Err(ActionError::UnsafePath(format!(
                "explicit target intersects Tonepoet recovery authority: {}",
                path.display()
            )));
        }
        if !filesystem.path_exists_no_follow(path)? {
            continue;
        }
        validate_target_under_subject(path, &subject)?;
        results.insert(path.clone(), true);
    }

    for (path, entry_type) in filesystem.enumerate_tree(&subject)? {
        let name_os = path.file_name().ok_or_else(|| {
            ActionError::UnsafePath(format!("enumerated path has no name: {}", path.display()))
        })?;
        // SR-1: wildcards never match hidden entries AND never descend into
        // hidden directories — a non-hidden name inside `.tonepoet-batch/`
        // is just as protected as the directory itself. Check every
        // component below the subject, not only the final name.
        let hidden_anywhere = path
            .strip_prefix(&subject)
            .unwrap_or(&path)
            .components()
            .any(|component| {
                matches!(component, Component::Normal(value) if is_hidden_name(value))
            });
        if hidden_anywhere {
            continue;
        }
        let name = name_os.to_string_lossy();
        let include = spec.target.iter().any(|pattern| {
            contains_wildcard(pattern)
                && (context.semantics.wildcard_matches)(pattern, &name)
        });
        if !include {
            continue;
        }
        if matches!(entry_type, ActionEntryType::Symlink | ActionEntryType::Other) {
            return Err(ActionError::UnsafePath(format!(
                "matched path is a symlink or special file: {}",
                path.display()
            )));
        }
        let excluded = spec
            .exclude
            .iter()
            .any(|pattern| (context.semantics.wildcard_matches)(pattern, &name));
        if excluded || protected_generated_wildcard_match(&path, context) {
            continue;
        }
        results.entry(path).or_insert(false);
    }

    for (pattern, path) in exact_patterns {
        if !filesystem.path_exists_no_follow(&path)? {
            continue;
        }
        let name = path
            .file_name()
            .map(|value| value.to_string_lossy())
            .unwrap_or_default();
        let exact_excluded = spec.exclude.iter().any(|exclude| {
            if contains_wildcard(exclude) {
                (context.semantics.wildcard_matches)(exclude, &name)
            } else {
                exclude.eq_ignore_ascii_case(&name)
            }
        });
        if exact_excluded {
            results.remove(&path);
        } else {
            results.insert(path, !contains_wildcard(&pattern));
        }
    }

    Ok(results
        .into_iter()
        .map(|(path, exact_target)| MatchedTarget { path, exact_target })
        .collect())
}


#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProtectedActionArtifactKind {
    CanonicalIdentity,
    ManualAuthority,
    JournalAuthority,
    LockAuthority,
    RecoveryWitness,
}

fn protected_action_artifact_kind(
    path: &Path,
    context: &ActionContext,
) -> Result<Option<ProtectedActionArtifactKind>, ActionError> {
    let path = lexical_normalize_absolute(path)?;
    let journal_dir = lexical_normalize_absolute(&context.journal_dir)?;
    if path == journal_dir || path.starts_with(&journal_dir) || journal_dir.starts_with(&path) {
        return Ok(Some(ProtectedActionArtifactKind::ManualAuthority));
    }
    if context.protected_generated_paths.iter().any(|protected| {
        protected
            .file_name()
            .is_some_and(|name| {
                name == OsStr::new(".tonepoet-action-identity.json")
                    || name == OsStr::new(".tonepoet-action-identity.import.json")
            })
            && paths_refer_to_same_location(&path, protected)
    }) {
        return Ok(Some(ProtectedActionArtifactKind::CanonicalIdentity));
    }

    for component in path.components() {
        let Component::Normal(name) = component else {
            continue;
        };
        let name = name.to_string_lossy();
        let kind = if matches!(
            name.as_ref(),
            ".tonepoet-action-identity.json"
                | ".tonepoet-action-identity.import.json"
                | ".tonepoet-action-identity.write.tmp"
        ) {
            Some(ProtectedActionArtifactKind::CanonicalIdentity)
        } else if matches!(
            name.as_ref(),
            ".tonepoet-actions-manual"
                | ".active-run.json"
                | ".active-run.write.tmp"
                | ".preview-authority.json"
                | ".preview-authority.write.tmp"
                | ".manual-run.lock"
        ) {
            Some(ProtectedActionArtifactKind::ManualAuthority)
        } else if matches!(
            name.as_ref(),
            ".tonepoet-action-journals" | ".tonepoet-actions"
        ) || (name.starts_with("actions-") && name.ends_with(".journal.json"))
            || (name.starts_with(".actions-") && name.ends_with(".write-tmp"))
        {
            Some(ProtectedActionArtifactKind::JournalAuthority)
        } else if matches!(
            name.as_ref(),
            ".tonepoet-action-locks" | ".tonepoet-action-lock-registry.lock"
        ) {
            Some(ProtectedActionArtifactKind::LockAuthority)
        } else if name.starts_with(INTERNAL_WORKSPACE_PREFIX)
            || name.starts_with(".tonepoet-action-witness-")
            || name.starts_with(".tonepoet-action-runtime-")
        {
            Some(ProtectedActionArtifactKind::RecoveryWitness)
        } else {
            None
        };
        if kind.is_some() {
            return Ok(kind);
        }
    }
    Ok(None)
}

fn reject_protected_action_artifact(
    path: &Path,
    context: &ActionContext,
    role: &str,
) -> Result<(), ActionError> {
    if let Some(kind) = protected_action_artifact_kind(path, context)? {
        return Err(ActionError::UnsafePath(format!(
            "{role} references protected Tonepoet control-plane authority ({kind:?}): {}",
            path.display()
        )));
    }
    Ok(())
}

fn path_intersects_reserved_action_authority(
    path: &Path,
    context: &ActionContext,
) -> Result<bool, ActionError> {
    Ok(protected_action_artifact_kind(path, context)?.is_some())
}

fn validate_target_patterns(spec: &TargetSpec) -> Result<(), ActionError> {
    for pattern in spec.target.iter().chain(&spec.exclude) {
        if pattern.trim().is_empty() {
            return Err(ActionError::Conflict(
                "include/exclude patterns may not be empty".to_string(),
            ));
        }
        if contains_wildcard(pattern) {
            let path = Path::new(pattern);
            if path.components().count() != 1
                || !matches!(path.components().next(), Some(Component::Normal(_)))
            {
                return Err(ActionError::Conflict(format!(
                    "wildcard patterns are file-name level and may not contain path separators: {pattern}"
                )));
            }
        }
    }
    Ok(())
}

fn collapse_descendant_targets(mut matches: Vec<MatchedTarget>) -> Vec<MatchedTarget> {
    matches.sort_by(|left, right| {
        left.path
            .components()
            .count()
            .cmp(&right.path.components().count())
            .then_with(|| left.path.cmp(&right.path))
    });
    let mut collapsed: Vec<MatchedTarget> = Vec::new();
    for matched in matches {
        if collapsed
            .iter()
            .any(|ancestor| matched.path != ancestor.path && matched.path.starts_with(&ancestor.path))
        {
            continue;
        }
        collapsed.push(matched);
    }
    collapsed
}

fn protected_generated_wildcard_match(path: &Path, context: &ActionContext) -> bool {
    if protected_action_artifact_kind(path, context)
        .map(|kind| kind.is_some())
        .unwrap_or(true)
    {
        return true;
    }
    if path
        .file_name()
        .map(|name| name.to_string_lossy().eq_ignore_ascii_case(GENERATED_CONVERSION_LOG))
        .unwrap_or(false)
    {
        return true;
    }
    context
        .protected_generated_paths
        .iter()
        .any(|protected| paths_refer_to_same_location(path, protected))
}

fn reject_protected_source(
    path: &Path,
    targeting: &TargetSpec,
    context: &ActionContext,
) -> Result<(), ActionError> {
    if targeting.allow_sources {
        return Ok(());
    }
    if context
        .protected_sources
        .iter()
        .any(|source| {
            paths_refer_to_same_location(path, source)
                || lexical_normalize_absolute(source)
                    .ok()
                    .zip(lexical_normalize_absolute(path).ok())
                    .map(|(source, target)| source.starts_with(&target))
                    .unwrap_or(false)
        })
    {
        return Err(ActionError::Conflict(format!(
            "source protection refused action on conversion input {} (set allow_sources = true to opt in)",
            path.display()
        )));
    }
    Ok(())
}

fn reject_wildcard_directory_with_hidden_descendants(
    filesystem: &dyn ActionFilesystem,
    matched: &MatchedTarget,
) -> Result<(), ActionError> {
    if matched.exact_target
        || filesystem.identity(&matched.path, true)?.kind != ObjectKind::Directory
    {
        return Ok(());
    }
    for (path, entry_type) in filesystem.enumerate_tree(&matched.path)? {
        let name = path.file_name().ok_or_else(|| {
            ActionError::UnsafePath(format!("enumerated path has no name: {}", path.display()))
        })?;
        if matches!(entry_type, ActionEntryType::Symlink | ActionEntryType::Other) {
            return Err(ActionError::UnsafePath(format!(
                "wildcard-selected directory contains a symlink or special file: {}",
                path.display()
            )));
        }
        if is_hidden_name(name) {
            return Err(ActionError::Conflict(format!(
                "wildcard-selected directory contains hidden entries and cannot be mutated as a unit: {}",
                matched.path.display()
            )));
        }
    }
    Ok(())
}

// Journal/execution/election implementation follows below.

fn capability_paths_for_operation(
    filesystem: &dyn ActionFilesystem,
    operation: &PlannedOperation,
) -> Result<Vec<ScopedPath>, ActionError> {
    match operation {
        PlannedOperation::RunScript {
            script,
            runtime_directory,
            ..
        } => Ok(vec![
            filesystem.scoped_path(script)?,
            filesystem.scoped_path(runtime_directory)?,
        ]),
        _ => operation
            .all_paths()
            .into_iter()
            .map(|path| filesystem.scoped_path(path))
            .collect(),
    }
}

fn journal_operations_for_plan(
    filesystem: &dyn ActionFilesystem,
    claim_id: &str,
    action_index: usize,
    plan: &ActionPlan,
) -> Result<Vec<JournalOperation>, ActionError> {
    plan.operations
        .iter()
        .enumerate()
        .map(|(operation_index, operation)| {
            Ok(JournalOperation {
                operation_id: operation_id(claim_id, action_index, operation_index),
                kind: operation.kind(),
                plan: operation.clone(),
                capability_paths: capability_paths_for_operation(filesystem, operation)?,
                state: OperationState::Prepared,
                observed_destination: None,
                result: None,
                script_execution: match operation {
                    PlannedOperation::RunScript {
                        runtime_directory,
                        containment_token,
                        ..
                    } => Some(ScriptExecutionJournal::new(
                        containment_token.clone(),
                        runtime_directory.clone(),
                    )),
                    _ => None,
                },
            })
        })
        .collect()
}

fn materialize_operation_roots(
    filesystem: &dyn ActionFilesystem,
    operations: &[JournalOperation],
) -> Result<(), ActionError> {
    let mut paths = BTreeSet::<PathBuf>::new();
    for operation in operations {
        match &operation.plan {
            PlannedOperation::Rename {
                destination,
                staging,
                ..
            } => {
                paths.insert(destination.clone());
                paths.insert(staging.clone());
            }
            PlannedOperation::Copy {
                destination,
                temporary,
                publication_witness,
                ..
            } => {
                paths.insert(destination.clone());
                paths.insert(temporary.clone());
                paths.insert(publication_witness.clone());
            }
            PlannedOperation::RepairCopyMetadata { .. } => {}
            PlannedOperation::Move {
                destination,
                temporary,
                publication_witness,
                source_witness,
                ..
            } => {
                paths.insert(destination.clone());
                paths.insert(temporary.clone());
                paths.insert(publication_witness.clone());
                paths.insert(source_witness.clone());
            }
            PlannedOperation::Delete { witness, .. } => {
                paths.insert(witness.clone());
            }
            PlannedOperation::CreateDirectory { path } => {
                paths.insert(path.clone());
            }
            PlannedOperation::RunScript { .. } => {}
        }
    }
    for path in paths {
        filesystem.materialize_root_for_path(&path, 0o755)?;
    }
    Ok(())
}

fn operation_roots_require_materialization(operations: &[JournalOperation]) -> bool {
    operations
        .iter()
        .any(|operation| {
            !matches!(
                operation.kind,
                OperationKind::RunScript | OperationKind::CopyMetadataRepair
            )
        })
}


// ---------------------------------------------------------------------------
// Durable journal
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum OperationKind {
    Rename,
    Copy,
    CopyMetadataRepair,
    Move,
    Delete,
    CreateDirectory,
    RunScript,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum OperationState {
    Prepared,
    DirectMoveStarted,
    DirectMoved,
    CopyStarted,
    CopyComplete,
    Verified,
    MetadataRepairStarted,
    MetadataRepaired,
    PublishStarted,
    Published,
    SourceStageStarted,
    SourceStaged,
    DisposalStarted,
    Disposed,
    RenameStageStarted,
    RenameStaged,
    RenamePublishStarted,
    RenamePublished,
    DirectoryCreateStarted,
    ScriptStartRecorded,
    ScriptCompleted,
    Committed,
    CleanupStarted,
    CleanupComplete,
    FailedDeterministic,
    InterruptedRecoverable,
    ManualRecoveryRequired,
    CancelledBeforeMutation,
}

impl OperationState {
    fn mutation_may_have_started(self) -> bool {
        !matches!(self, Self::Prepared | Self::CancelledBeforeMutation)
    }

    fn terminal(self) -> bool {
        matches!(
            self,
            Self::CleanupComplete
                | Self::FailedDeterministic
                | Self::ManualRecoveryRequired
                | Self::CancelledBeforeMutation
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum JournalActionState {
    Pending,
    Planned,
    Running,
    Completed,
    NoOp,
    FailedDeterministic,
    SkippedAfterFailure,
    CancelledBeforeMutation,
    InterruptedRecoverable,
    ManualRecoveryRequired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RootMaterializationState {
    NotStarted,
    NotRequired,
    Started,
    Complete,
}

impl Default for RootMaterializationState {
    fn default() -> Self {
        Self::NotStarted
    }
}

impl RootMaterializationState {
    fn mutation_may_have_started(self) -> bool {
        matches!(self, Self::Started | Self::Complete)
    }
}

impl JournalActionState {
    fn terminal(self) -> bool {
        matches!(
            self,
            Self::Completed
                | Self::NoOp
                | Self::FailedDeterministic
                | Self::SkippedAfterFailure
                | Self::CancelledBeforeMutation
                | Self::ManualRecoveryRequired
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ScriptTerminationJournal {
    reason: TerminationReason,
    graceful_deadline_unix_millis: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ScriptTerminalState {
    Success,
    ExitFailure,
    TimedOut,
    CancelledAfterStart,
    BackgroundDescendants,
    ContainmentUncertain,
    SetupFailedBeforeExecution,
    ManualRecoveryRequired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ScriptExecutionJournal {
    schema_version: u32,
    token: String,
    runtime_directory: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    runtime_identity: Option<RuntimeDirectoryIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    descriptor: Option<ContainmentDescriptor>,
    start_committed: bool,
    user_code_released: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    termination_requested: Option<ScriptTerminationJournal>,
    forced_termination_requested: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    leader_exit_status: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    containment_empty: Option<ContainmentConfidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    output_capture: Option<OutputCaptureSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    terminal: Option<ScriptTerminalState>,
    cleanup_complete: bool,
}

impl ScriptExecutionJournal {
    fn new(token: String, runtime_directory: PathBuf) -> Self {
        Self {
            schema_version: SCRIPT_EXECUTION_SCHEMA_VERSION,
            token,
            runtime_directory,
            runtime_identity: None,
            descriptor: None,
            start_committed: false,
            user_code_released: false,
            termination_requested: None,
            forced_termination_requested: false,
            leader_exit_status: None,
            containment_empty: None,
            output_capture: None,
            terminal: None,
            cleanup_complete: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct JournalOperation {
    operation_id: String,
    kind: OperationKind,
    plan: PlannedOperation,
    capability_paths: Vec<ScopedPath>,
    state: OperationState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    observed_destination: Option<ObjectIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    result: Option<OperationResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    script_execution: Option<ScriptExecutionJournal>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct JournalAction {
    index: usize,
    action_serialized: String,
    action_sha256: String,
    continue_on_error: bool,
    state: JournalActionState,
    /// Destination-root creation is itself a filesystem mutation. Persist its
    /// start before calling `create_dir_all`-equivalent capability operations
    /// so cancellation and recovery can never describe a partially-created
    /// root as "before mutation" or merely "planned".
    root_materialization: RootMaterializationState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    plan: Option<ActionPlan>,
    #[serde(default)]
    operations: Vec<JournalOperation>,
    #[serde(default)]
    notices: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

fn journal_action_mutation_may_have_started(action: &JournalAction) -> bool {
    action.root_materialization.mutation_may_have_started()
        || action
            .operations
            .iter()
            .any(|operation| operation.state.mutation_may_have_started())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CancellationRecord {
    requested: bool,
    before_any_mutation: bool,
    recovery_required: bool,
}

impl Default for CancellationRecord {
    fn default() -> Self {
        Self {
            requested: false,
            before_any_mutation: false,
            recovery_required: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct StopDecision {
    failed_action_index: usize,
    remainder_marked_skipped: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct TerminalJournalResult {
    report: ActionPhaseReport,
    cleanup_complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ActionJournal {
    schema_version: u32,
    generation: u64,
    run_identity: String,
    album_identity: String,
    phase: ActionPhase,
    pipeline_serialized: String,
    pipeline_sha256: String,
    claim_id: String,
    subject_dir: PathBuf,
    source_path: PathBuf,
    output_root: PathBuf,
    album_dir: PathBuf,
    journal_path: PathBuf,
    journal_write_temporary: PathBuf,
    journal_scoped_path: ScopedPath,
    journal_write_temporary_scoped_path: ScopedPath,
    capability_roots: Vec<ScopeRecord>,
    workspace_paths: Vec<PathBuf>,
    workspace_capability_paths: Vec<ScopedPath>,
    actions: Vec<JournalAction>,
    cancellation: CancellationRecord,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stop_decision: Option<StopDecision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    terminal: Option<TerminalJournalResult>,
}

fn deserialize_action_journal(bytes: &[u8]) -> Result<ActionJournal, ActionError> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(ActionError::Serialization)?;
    let schema = value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| ActionError::InvalidJournal(
            "action journal is missing an integer schema_version".to_string(),
        ))?;
    if schema == 2 {
        return Err(ActionError::InvalidJournal(
            "found a pass-1 pathname-authority action journal (schema 2); descriptor-safe recovery refuses to reinterpret it automatically. Preserve its artifacts and complete administrative recovery before retrying".to_string(),
        ));
    }
    if schema == 3 {
        return Err(ActionError::InvalidJournal(
            "found a descriptor-capability action journal from before durable runscript containment lifecycle recording (schema 3). Any recorded script start is non-replayable and cannot authorize local signalling; preserve the journal and complete administrative recovery".to_string(),
        ));
    }
    if schema == 4 {
        return Err(ActionError::InvalidJournal(
            "found a runscript journal from the earlier supervisor draft (schema 4). It does not durably bind the runtime directory descriptor, complete termination escalation, containment-empty proof, and output-terminal state required by the current protocol. Automatic replay or PID-based recovery is refused; preserve the journal and complete administrative recovery".to_string(),
        ));
    }
    if schema == 5 {
        return Err(ActionError::InvalidJournal(
            "found an action journal from before destination-root materialization became a first-class mutation (schema 5). It cannot prove whether configured roots were partially created before interruption; preserve the journal and artifacts and complete administrative recovery".to_string(),
        ));
    }
    if schema == 8 {
        return Err(ActionError::InvalidJournal(
            "found an action journal from before sibling destination roots shared one durable first-materialization authority (schema 8). Its independent descendant tokens can contradict one another after a common missing parent is published; preserve the journal and artifacts and complete administrative recovery".to_string(),
        ));
    }
    // Deserialize from the original bytes, not the schema-sniffing Value:
    // serde_json::Value cannot represent the journal's 128-bit nanosecond
    // fields ("i128 is not supported").
    serde_json::from_slice(bytes).map_err(ActionError::Serialization)
}

struct JournalStore<'a> {
    path: PathBuf,
    temporary: PathBuf,
    filesystem: &'a dyn ActionFilesystem,
}

fn journal_write_temporary_path(path: &Path) -> Result<PathBuf, ActionError> {
    let file_name = path
        .file_name()
        .ok_or_else(|| ActionError::UnsafePath("journal path has no file name".to_string()))?
        .to_string_lossy();
    Ok(path.with_file_name(format!(".{file_name}.write-tmp")))
}

fn select_loaded_journal(
    final_bytes: Option<Vec<u8>>,
    temporary_bytes: Option<Vec<u8>>,
) -> Result<Option<(ActionJournal, bool)>, ActionError> {
    match (final_bytes, temporary_bytes) {
        (None, None) => Ok(None),
        (Some(bytes), None) => Ok(Some((deserialize_action_journal(&bytes)?, false))),
        (None, Some(bytes)) => Ok(Some((deserialize_action_journal(&bytes)?, true))),
        (Some(final_bytes), Some(temporary_bytes)) => {
            let final_journal = deserialize_action_journal(&final_bytes)?;
            let temporary_journal = deserialize_action_journal(&temporary_bytes)?;
            validate_owned_journal_generation(&temporary_journal, &final_journal)?;
            if temporary_journal.generation > final_journal.generation {
                Ok(Some((temporary_journal, true)))
            } else if final_journal.generation > temporary_journal.generation {
                Ok(Some((final_journal, false)))
            } else if temporary_journal == final_journal {
                Ok(Some((final_journal, false)))
            } else {
                Err(ActionError::Contradiction(
                    "journal and write temporary have the same generation but different contents"
                        .to_string(),
                ))
            }
        }
    }
}

fn load_journal_bootstrap(
    store: &JournalStore<'_>,
) -> Result<Option<(ActionJournal, bool)>, ActionError> {
    select_loaded_journal(
        store.filesystem.bootstrap_read_optional(&store.path)?,
        store.filesystem.bootstrap_read_optional(&store.temporary)?,
    )
}

fn load_journal_bound(
    store: &JournalStore<'_>,
) -> Result<Option<(ActionJournal, bool)>, ActionError> {
    let final_bytes = store
        .filesystem
        .read_bytes_with_identity_optional(&store.path)?
        .map(|(bytes, _identity)| bytes);
    let temporary_bytes = store
        .filesystem
        .read_bytes_with_identity_optional(&store.temporary)?
        .map(|(bytes, _identity)| bytes);
    select_loaded_journal(final_bytes, temporary_bytes)
}

fn validate_owned_journal_generation(
    candidate: &ActionJournal,
    authority: &ActionJournal,
) -> Result<(), ActionError> {
    if candidate.schema_version != JOURNAL_SCHEMA_VERSION
        || candidate.run_identity != authority.run_identity
        || candidate.album_identity != authority.album_identity
        || candidate.phase != authority.phase
        || candidate.pipeline_serialized != authority.pipeline_serialized
        || candidate.pipeline_sha256 != authority.pipeline_sha256
        || candidate.claim_id != authority.claim_id
        || candidate.journal_path != authority.journal_path
        || candidate.journal_write_temporary != authority.journal_write_temporary
        || candidate.journal_scoped_path != authority.journal_scoped_path
        || candidate.journal_write_temporary_scoped_path
            != authority.journal_write_temporary_scoped_path
    {
        return Err(ActionError::Contradiction(
            "journal write temporary belongs to a foreign or contradictory generation"
                .to_string(),
        ));
    }
    validate_scope_record_generations(candidate, authority)
}

fn validate_scope_record_generations(
    candidate: &ActionJournal,
    authority: &ActionJournal,
) -> Result<(), ActionError> {
    if candidate.capability_roots.len() != authority.capability_roots.len() {
        return Err(ActionError::Contradiction(
            "journal generations have different capability-root sets".to_string(),
        ));
    }
    let candidate_roots: BTreeMap<_, _> = candidate
        .capability_roots
        .iter()
        .cloned()
        .map(|record| (record.id.clone(), record))
        .collect();
    let authority_roots: BTreeMap<_, _> = authority
        .capability_roots
        .iter()
        .cloned()
        .map(|record| (record.id.clone(), record))
        .collect();
    if candidate_roots.len() != candidate.capability_roots.len()
        || authority_roots.len() != authority.capability_roots.len()
        || candidate_roots.keys().ne(authority_roots.keys())
    {
        return Err(ActionError::Contradiction(
            "journal generations contain duplicate or different capability scopes".to_string(),
        ));
    }
    let (newer, older) = if candidate.generation >= authority.generation {
        (&candidate_roots, &authority_roots)
    } else {
        (&authority_roots, &candidate_roots)
    };
    for (id, newer_record) in newer {
        let older_record = older.get(id).ok_or_else(|| {
            ActionError::Contradiction("capability scope vanished across journal generations".to_string())
        })?;
        let immutable_equal = newer_record.id == older_record.id
            && newer_record.acquisition_path == older_record.acquisition_path
            && newer_record.logical_path == older_record.logical_path
            && newer_record.base_relative == older_record.base_relative
            && newer_record.materialization_token == older_record.materialization_token
            && newer_record.materialization_authority_name
                == older_record.materialization_authority_name
            && newer_record.device == older_record.device
            && newer_record.inode == older_record.inode;
        if !immutable_equal {
            return Err(ActionError::Contradiction(format!(
                "capability scope {} changed authority across journal generations",
                id.as_str()
            )));
        }
        let newer_materialized = (newer_record.materialized_device, newer_record.materialized_inode);
        let older_materialized = (older_record.materialized_device, older_record.materialized_inode);
        let legal = match (older_materialized, newer_materialized) {
            ((None, None), (None, None)) => true,
            ((None, None), (Some(_), Some(_))) => true,
            ((Some(old_device), Some(old_inode)), (Some(new_device), Some(new_inode))) => {
                old_device == new_device && old_inode == new_inode
            }
            _ => false,
        };
        if !legal || (candidate.generation == authority.generation && newer_record != older_record) {
            return Err(ActionError::Contradiction(format!(
                "capability scope {} has an illegal materialization transition",
                id.as_str()
            )));
        }
    }
    Ok(())
}

fn read_retained_journal_file(
    filesystem: &dyn ActionFilesystem,
    path: &Path,
) -> Result<Option<(ActionJournal, CapEntryIdentity)>, ActionError> {
    match filesystem.read_bytes_with_identity_optional(path)? {
        Some((bytes, identity)) => Ok(Some((deserialize_action_journal(&bytes)?, identity))),
        None => Ok(None),
    }
}

impl<'a> JournalStore<'a> {
    fn new(path: PathBuf, filesystem: &'a dyn ActionFilesystem) -> Result<Self, ActionError> {
        let temporary = journal_write_temporary_path(&path)?;
        Ok(Self { path, temporary, filesystem })
    }

    fn reconcile_loaded(&self, journal: &ActionJournal) -> Result<(), ActionError> {
        if journal.journal_path != self.path || journal.journal_write_temporary != self.temporary {
            return Err(ActionError::InvalidJournal(
                "journal store paths do not match journal authority".to_string(),
            ));
        }
        let final_file = read_retained_journal_file(self.filesystem, &self.path)?;
        let temporary_file = read_retained_journal_file(self.filesystem, &self.temporary)?;
        match (final_file, temporary_file) {
            (Some((final_journal, _final_identity)), None) if final_journal == *journal => Ok(()),
            (None, Some((temporary_journal, temporary_identity)))
                if temporary_journal == *journal =>
            {
                self.filesystem.replace_owned_regular(
                    &self.temporary,
                    &self.path,
                    temporary_identity,
                    None,
                )?;
                self.filesystem.sync_parent(&self.path)
            }
            (Some((final_journal, final_identity)), Some((temporary_journal, temporary_identity))) => {
                validate_owned_journal_generation(&temporary_journal, &final_journal)?;
                if temporary_journal == *journal
                    && temporary_journal.generation >= final_journal.generation
                {
                    self.filesystem.replace_owned_regular(
                        &self.temporary,
                        &self.path,
                        temporary_identity,
                        Some(final_identity),
                    )?;
                    self.filesystem.sync_parent(&self.path)
                } else if final_journal == *journal
                    && temporary_journal.generation < final_journal.generation
                {
                    self.filesystem
                        .remove_owned_path(&self.temporary, temporary_identity)?;
                    self.filesystem.sync_parent(&self.temporary)
                } else {
                    Err(ActionError::Contradiction(
                        "journal files changed between bootstrap selection and reconciliation"
                            .to_string(),
                    ))
                }
            }
            _ => Err(ActionError::Contradiction(
                "selected journal generation vanished or changed before reconciliation"
                    .to_string(),
            )),
        }
    }

    fn persist(&self, journal: &mut ActionJournal) -> Result<(), ActionError> {
        #[cfg(test)]
        test_maybe_fail_journal_persist()?;
        if journal.journal_path != self.path || journal.journal_write_temporary != self.temporary {
            return Err(ActionError::InvalidJournal(
                "journal store paths do not match journal authority".to_string(),
            ));
        }
        if self.filesystem.path_exists_no_follow(&self.temporary)? {
            return Err(ActionError::Contradiction(
                "journal write temporary was not reconciled before advancing generation"
                    .to_string(),
            ));
        }
        let parent = self.path.parent().ok_or_else(|| {
            ActionError::UnsafePath(format!("journal path has no parent: {}", self.path.display()))
        })?;
        // The recovery workspace may itself be a previously absent logical
        // root. Materialize that explicitly, then create its child directories
        // before serializing so this generation durably binds the descriptor
        // identity that receives the journal. Ordinary child-mutation APIs never
        // materialize a missing logical root implicitly.
        self.filesystem.materialize_root_for_path(parent, 0o700)?;
        self.filesystem.create_private_dir_all(parent)?;
        journal.capability_roots = self.filesystem.scope_records()?;
        journal.generation = journal.generation.checked_add(1).ok_or_else(|| {
            ActionError::InvalidJournal("journal generation counter overflow".to_string())
        })?;
        let bytes = serde_json::to_vec_pretty(journal).map_err(ActionError::Serialization)?;
        self.filesystem
            .write_bytes_create_new_durable(&self.temporary, &bytes)?;
        let temporary_identity = self
            .filesystem
            .entry_identity(&self.temporary)?
            .ok_or_else(|| ActionError::Contradiction(
                "journal write temporary vanished after durable creation".to_string(),
            ))?;
        let destination_identity = self.filesystem.entry_identity(&self.path)?;
        self.filesystem.replace_owned_regular(
            &self.temporary,
            &self.path,
            temporary_identity,
            destination_identity,
        )?;
        self.filesystem.sync_parent(&self.path)?;
        self.filesystem.finalize_materialized_roots()?;
        Ok(())
    }
}

impl<'a> ActionEngine<'a> {
    pub fn execute_phase(
        &self,
        pipeline: &ActionPipeline,
        context: &ActionContext,
        cancellation: &dyn ActionCancellation,
    ) -> Result<ActionPhaseReport, ActionError> {
        self.execute_phase_internal(pipeline, context, cancellation, None)
    }

    fn validate_prepared_plans_against_pipeline(
        &self,
        pipeline: &ActionPipeline,
        context: &ActionContext,
        claim_id: &str,
        plans: &[ActionPlan],
    ) -> Result<(), ActionError> {
        let configured = pipeline.for_phase(context.phase);
        if claim_id.is_empty() || plans.len() != configured.len() {
            return Err(ActionError::InvalidJournal(
                "prepared explicit action plan does not match the configured phase".to_string(),
            ));
        }
        for (action_index, (action, plan)) in configured.iter().zip(plans).enumerate() {
            if plan.action_kind != action.kind_name() {
                return Err(ActionError::InvalidJournal(format!(
                    "prepared action {} kind does not match the configured pipeline",
                    action_index + 1
                )));
            }
            validate_planning_precondition_shapes(action, plan, context)?;
            let operations = journal_operations_for_plan(
                self.filesystem,
                claim_id,
                action_index,
                plan,
            )?;
            for (operation_index, operation) in operations.iter().enumerate() {
                validate_operation_paths(
                    operation,
                    action,
                    context,
                    claim_id,
                    action_index,
                    operation_index,
                    false,
                )?;
                let expected_capability_paths =
                    capability_paths_for_operation(self.filesystem, &operation.plan)?;
                if operation.capability_paths != expected_capability_paths {
                    return Err(ActionError::InvalidJournal(format!(
                        "prepared action {} operation {} capability authority mismatch",
                        action_index + 1,
                        operation_index + 1
                    )));
                }
            }
        }
        Ok(())
    }

    fn execute_phase_with_prepared_plans(
        &self,
        pipeline: &ActionPipeline,
        context: &ActionContext,
        cancellation: &dyn ActionCancellation,
        claim_id: &str,
        plans: &[ActionPlan],
    ) -> Result<ActionPhaseReport, ActionError> {
        self.execute_phase_internal(
            pipeline,
            context,
            cancellation,
            Some((claim_id, plans)),
        )
    }

    fn execute_phase_internal(
        &self,
        pipeline: &ActionPipeline,
        context: &ActionContext,
        cancellation: &dyn ActionCancellation,
        prepared: Option<(&str, &[ActionPlan])>,
    ) -> Result<ActionPhaseReport, ActionError> {
        validate_context_syntax(context)?;
        let actions = pipeline.for_phase(context.phase);
        if actions.is_empty() {
            if context_has_retained_capabilities(context) {
                prepare_and_validate_context_capabilities(self.filesystem, context)?;
            } else {
                validate_context(context)?;
            }
            return Ok(ActionPhaseReport {
                phase: Some(context.phase),
                ..ActionPhaseReport::default()
            });
        }
        if let Some((claim_id, plans)) = prepared {
            if claim_id.is_empty() || plans.len() != actions.len() {
                return Err(ActionError::InvalidJournal(
                    "prepared explicit action plan does not match the configured phase".to_string(),
                ));
            }
        }
        let pipeline_serialized = pipeline.canonical_serialization()?;
        let pipeline_sha256 = sha256_hex(pipeline_serialized.as_bytes());
        let journal_path = action_journal_path(context, &pipeline_sha256)?;
        let store = JournalStore::new(journal_path.clone(), self.filesystem)?;
        let retained_live_context = prepare_context_for_journal_read(self.filesystem, context)?;
        let loaded_journal = if retained_live_context {
            load_journal_bound(&store)?
        } else {
            load_journal_bootstrap(&store)?
        };
        let had_loaded_journal = loaded_journal.is_some();
        let mut journal = if let Some((journal, _from_write_temporary)) = loaded_journal {
            if retained_live_context {
                prepare_retained_pipeline_capabilities(self.filesystem, pipeline, context)?;
                self.filesystem.restore_scope_records(
                    &journal.capability_roots,
                    &expected_capability_roots(pipeline, context, &journal.capability_roots)?,
                )?;
                prepare_pipeline_capabilities(self.filesystem, pipeline, context)?;
                self.filesystem.validate_scope_records(&journal.capability_roots)?;
            } else {
                self.filesystem.restore_scope_records(
                    &journal.capability_roots,
                    &expected_capability_roots(pipeline, context, &journal.capability_roots)?,
                )?;
                prepare_context_capabilities(self.filesystem, context)?;
                validate_context_through_capabilities(self.filesystem, context)?;
                prepare_pipeline_capabilities(self.filesystem, pipeline, context)?;
            }
            validate_journal(
                &journal,
                self.filesystem,
                context,
                pipeline,
                &pipeline_serialized,
                &pipeline_sha256,
            )?;
            if let Some((expected_claim, expected_plans)) = prepared {
                let journal_plans = journal
                    .actions
                    .iter()
                    .map(|action| action.plan.clone())
                    .collect::<Option<Vec<_>>>()
                    .ok_or_else(|| ActionError::InvalidJournal(
                        "durable explicit journal is missing its reviewed plan".to_string(),
                    ))?;
                if journal.claim_id != expected_claim || journal_plans != expected_plans {
                    return Err(ActionError::Contradiction(
                        "durable explicit journal does not match the reviewed plan authority".to_string(),
                    ));
                }
            }
            journal
        } else {
            if !retained_live_context {
                prepare_context_capabilities(self.filesystem, context)?;
                validate_context_through_capabilities(self.filesystem, context)?;
            }
            prepare_pipeline_capabilities(self.filesystem, pipeline, context)?;
            let claim_id = prepared
                .map(|(claim_id, _)| claim_id.to_string())
                .unwrap_or_else(|| Uuid::new_v4().to_string());
            let mut journal_actions = Vec::with_capacity(actions.len());
            for (index, action) in actions.iter().enumerate() {
                let serialized = serde_json::to_string(action)
                    .map_err(ActionError::Serialization)?;
                let prepared_plan = prepared.map(|(_, plans)| plans[index].clone());
                let operations = match prepared_plan.as_ref() {
                    Some(plan) => journal_operations_for_plan(
                        self.filesystem,
                        &claim_id,
                        index,
                        plan,
                    )?,
                    None => Vec::new(),
                };
                journal_actions.push(JournalAction {
                    index,
                    action_sha256: sha256_hex(serialized.as_bytes()),
                    action_serialized: serialized,
                    continue_on_error: action.continue_on_error(),
                    state: if prepared_plan.is_some() {
                        JournalActionState::Planned
                    } else {
                        JournalActionState::Pending
                    },
                    root_materialization: RootMaterializationState::NotStarted,
                    notices: prepared_plan
                        .as_ref()
                        .map(|plan| plan.notices.clone())
                        .unwrap_or_default(),
                    plan: prepared_plan,
                    operations,
                    error: None,
                });
            }
            let mut journal = ActionJournal {
                schema_version: JOURNAL_SCHEMA_VERSION,
                generation: 0,
                run_identity: context.run_identity.clone(),
                album_identity: context.album_identity.clone(),
                phase: context.phase,
                pipeline_serialized: pipeline_serialized.clone(),
                pipeline_sha256: pipeline_sha256.clone(),
                claim_id,
                subject_dir: context.subject_dir.clone(),
                source_path: context.source_path.clone(),
                output_root: context.output_root.clone(),
                album_dir: context.album_dir.clone(),
                journal_path: journal_path.clone(),
                journal_write_temporary: store.temporary.clone(),
                journal_scoped_path: self.filesystem.scoped_path(&journal_path)?,
                journal_write_temporary_scoped_path: self.filesystem.scoped_path(&store.temporary)?,
                capability_roots: self.filesystem.scope_records()?,
                workspace_paths: Vec::new(),
                workspace_capability_paths: Vec::new(),
                actions: journal_actions,
                cancellation: CancellationRecord::default(),
                stop_decision: None,
                terminal: None,
            };
            if prepared.is_some() {
                for action_index in 0..journal.actions.len() {
                    register_journal_workspace_paths(
                        self.filesystem,
                        &mut journal,
                        action_index,
                    )?;
                }
            }
            // This durable write precedes every action mutation and contains
            // the complete pipeline/action slot identity, including every
            // reviewed operation for an explicit prepared invocation.
            store.persist(&mut journal)?;
            journal
        };

        if had_loaded_journal {
            // Reconcile an authoritative write-temporary without first deleting
            // it. This closes the power-loss window that would otherwise allow
            // recovery to fall back to an older final journal generation.
            store.reconcile_loaded(&journal)?;
        }

        if let Some(terminal) = &journal.terminal {
            validate_terminal_report(&journal, terminal)?;
            if terminal.cleanup_complete {
                prune_action_journal_retention_best_effort(context);
                return Ok(terminal.report.clone());
            }
        }

        // Validate the journal's report shape up front; every consuming
        // branch rebuilds the definitive report from the settled journal.
        let _ = report_from_journal(&journal)?;

        // Automatic conversion actions join the shared registry once for the
        // complete configured phase before the first user-library mutation.
        // Control-plane journal reconciliation above does not mutate user data.
        admit_conversion_action_phase_claims(pipeline, context, prepared.is_some())?;

        for action_index in 0..journal.actions.len() {
            if journal.actions[action_index].state.terminal() {
                continue;
            }
            if journal.stop_decision.is_some() {
                mark_remainder_skipped(&store, &mut journal, action_index)?;
                break;
            }
            if cancellation.is_cancelled()
                && !journal_action_mutation_may_have_started(
                    &journal.actions[action_index],
                )
            {
                journal.cancellation = CancellationRecord {
                    requested: true,
                    before_any_mutation: !journal.actions[..action_index]
                        .iter()
                        .any(journal_action_mutation_may_have_started),
                    recovery_required: false,
                };
                journal.actions[action_index].state =
                    JournalActionState::CancelledBeforeMutation;
                journal.actions[action_index].error =
                    Some("cancelled before action mutation".to_string());
                for later in journal.actions.iter_mut().skip(action_index + 1) {
                    later.state = JournalActionState::SkippedAfterFailure;
                    later.error = Some("phase cancelled before mutation".to_string());
                }
                store.persist(&mut journal)?;
                let mut report = report_from_journal(&journal)?;
                report.phase = Some(context.phase);
                report.cancelled = true;
                finalize_terminal(&store, &mut journal, report.clone(), true)?;
                return Err(ActionError::CancelledBeforeMutation(format!(
                    "{} action {}",
                    context.phase.as_str(),
                    action_index + 1
                )));
            }
            if cancellation.is_cancelled()
                && journal_action_mutation_may_have_started(
                    &journal.actions[action_index],
                )
            {
                journal.cancellation = CancellationRecord {
                    requested: true,
                    before_any_mutation: false,
                    recovery_required: true,
                };
                journal.actions[action_index].state =
                    JournalActionState::InterruptedRecoverable;
                journal.actions[action_index].error = Some(
                    "cancelled after durable action mutation state was recorded".to_string(),
                );
                store.persist(&mut journal)?;
                return Err(ActionError::Interrupted(format!(
                    "cancellation after action {} mutation began",
                    action_index + 1
                )));
            }

            if journal.actions[action_index].plan.is_none() {
                let action = &actions[action_index];
                match self.plan_action(action_index, action, context, &journal.claim_id) {
                    Ok(plan) => {
                        let operations = journal_operations_for_plan(
                            self.filesystem,
                            &journal.claim_id,
                            action_index,
                            &plan,
                        )?;
                        journal.actions[action_index].notices = plan.notices.clone();
                        journal.actions[action_index].operations = operations;
                        journal.actions[action_index].plan = Some(plan);
                        journal.actions[action_index].state = JournalActionState::Planned;
                        register_journal_workspace_paths(self.filesystem, &mut journal, action_index)?;
                        store.persist(&mut journal)?;
                    }
                    Err(error) => {
                        let deterministic = error.deterministic();
                        record_action_failure(
                            &store,
                            &mut journal,
                            action_index,
                            error.to_string(),
                            deterministic,
                        )?;
                        if !deterministic {
                            return Err(ActionError::Interrupted(format!(
                                "planning action {} could not complete durably: {error}",
                                action_index + 1
                            )));
                        }
                        if !journal.actions[action_index].continue_on_error {
                            mark_stop_after_failure(&store, &mut journal, action_index)?;
                            break;
                        }
                        continue;
                    }
                }
            }

            if journal.actions[action_index].operations.is_empty() {
                let plan = journal.actions[action_index]
                    .plan
                    .as_ref()
                    .ok_or_else(|| ActionError::InvalidJournal(
                        "no-op action is missing its durable plan".to_string(),
                    ))?;
                if let Err(error) = validate_planning_preconditions(
                    self.filesystem,
                    std::slice::from_ref(plan),
                ) {
                    let deterministic = error.deterministic();
                    record_action_failure(
                        &store,
                        &mut journal,
                        action_index,
                        error.to_string(),
                        deterministic,
                    )?;
                    if !deterministic {
                        return Err(ActionError::Interrupted(format!(
                            "no-op validation for action {} could not complete durably: {error}",
                            action_index + 1
                        )));
                    }
                    if !journal.actions[action_index].continue_on_error {
                        mark_stop_after_failure(&store, &mut journal, action_index)?;
                        break;
                    }
                    continue;
                }
                journal.actions[action_index].state = JournalActionState::NoOp;
                store.persist(&mut journal)?;
                continue;
            }

            if let Some(plan) = journal.actions[action_index].plan.as_ref() {
                assert_conversion_action_plan_is_admitted(plan, context, prepared.is_some())?;
            }

            if cancellation.is_cancelled()
                && !journal_action_mutation_may_have_started(
                    &journal.actions[action_index],
                )
            {
                let error = ActionError::CancelledBeforeMutation(format!(
                    "{} action {} before destination-root materialization",
                    context.phase.as_str(),
                    action_index + 1
                ));
                finalize_cancelled_before_mutation(
                    &store,
                    &mut journal,
                    action_index,
                    error.to_string(),
                )?;
                return Err(error);
            }

            // A configured destination root may not have existed when its
            // capability was acquired. The complete plan is already durable;
            // materialize and retain only roots used by this non-empty action,
            // then persist their device/inode identities before any operand
            // beneath them is created, renamed, or removed.
            journal.actions[action_index].state = JournalActionState::Running;
            if operation_roots_require_materialization(
                &journal.actions[action_index].operations,
            ) {
                journal.actions[action_index].root_materialization =
                    RootMaterializationState::Started;
                store.persist(&mut journal)?;
                if let Err(error) = materialize_operation_roots(
                    self.filesystem,
                    &journal.actions[action_index].operations,
                ) {
                    journal.actions[action_index].state =
                        JournalActionState::InterruptedRecoverable;
                    journal.actions[action_index].error = Some(format!(
                        "destination-root materialization stopped after mutation was durably recorded: {error}"
                    ));
                    journal.cancellation.recovery_required = true;
                    store.persist(&mut journal)?;
                    return Err(ActionError::Interrupted(format!(
                        "action {} destination-root materialization requires recovery: {error}",
                        action_index + 1
                    )));
                }
                journal.actions[action_index].root_materialization =
                    RootMaterializationState::Complete;
            } else {
                journal.actions[action_index].root_materialization =
                    RootMaterializationState::NotRequired;
            }
            store.persist(&mut journal)?;

            if cancellation.is_cancelled() {
                if journal_action_mutation_may_have_started(
                    &journal.actions[action_index],
                ) {
                    journal.cancellation = CancellationRecord {
                        requested: true,
                        before_any_mutation: false,
                        recovery_required: true,
                    };
                    journal.actions[action_index].state =
                        JournalActionState::InterruptedRecoverable;
                    journal.actions[action_index].error = Some(
                        "cancelled after destination-root materialization began".to_string(),
                    );
                    store.persist(&mut journal)?;
                    return Err(ActionError::Interrupted(format!(
                        "cancellation after action {} destination-root mutation began",
                        action_index + 1
                    )));
                }
                let error = ActionError::CancelledBeforeMutation(format!(
                    "{} action {} before operation start",
                    context.phase.as_str(),
                    action_index + 1
                ));
                finalize_cancelled_before_mutation(
                    &store,
                    &mut journal,
                    action_index,
                    error.to_string(),
                )?;
                return Err(error);
            }
            let apply_result = if journal.actions[action_index]
                .operations
                .iter()
                .all(|operation| operation.kind == OperationKind::Rename)
            {
                self.apply_rename_transaction(
                    &store,
                    &mut journal,
                    action_index,
                    cancellation,
                )
            } else {
                self.apply_action_operations(
                    &store,
                    &mut journal,
                    action_index,
                    cancellation,
                )
            };

            match apply_result {
                Ok(()) => {
                    journal.actions[action_index].state = JournalActionState::Completed;
                    journal.actions[action_index].error = None;
                    store.persist(&mut journal)?;
                }
                Err(error @ ActionError::CancelledBeforeMutation(_)) => {
                    finalize_cancelled_before_mutation(
                        &store,
                        &mut journal,
                        action_index,
                        error.to_string(),
                    )?;
                    return Err(error);
                }
                Err(error @ ActionError::ManualRecoveryRequired(_)) => {
                    mark_action_operation_terminal(
                        &store,
                        &mut journal,
                        action_index,
                        OperationState::ManualRecoveryRequired,
                        OperationResultStatus::ManualRecoveryRequired,
                        error.to_string(),
                    )?;
                    journal.actions[action_index].state =
                        JournalActionState::ManualRecoveryRequired;
                    journal.actions[action_index].error = Some(error.to_string());
                    journal.cancellation.recovery_required = true;
                    store.persist(&mut journal)?;
                    return Err(error);
                }
                Err(error @ ActionError::Contradiction(_))
                | Err(error @ ActionError::InvalidJournal(_)) => {
                    mark_action_operation_terminal(
                        &store,
                        &mut journal,
                        action_index,
                        OperationState::ManualRecoveryRequired,
                        OperationResultStatus::ManualRecoveryRequired,
                        error.to_string(),
                    )?;
                    journal.actions[action_index].state =
                        JournalActionState::ManualRecoveryRequired;
                    journal.actions[action_index].error = Some(error.to_string());
                    journal.cancellation.recovery_required = true;
                    store.persist(&mut journal)?;
                    return Err(ActionError::ManualRecoveryRequired(error.to_string()));
                }
                Err(error @ ActionError::Interrupted(_)) => {
                    // Preserve the exact durable operation stage. Recovery
                    // resumes from that state; replacing it with a generic
                    // interrupted marker would discard the state-machine
                    // authority needed to continue safely.
                    journal.actions[action_index].state =
                        JournalActionState::InterruptedRecoverable;
                    journal.actions[action_index].error = Some(error.to_string());
                    journal.cancellation.recovery_required = true;
                    store.persist(&mut journal)?;
                    return Err(error);
                }
                Err(error) => {
                    let mutation_started = journal_action_mutation_may_have_started(
                        &journal.actions[action_index],
                    );
                    if mutation_started
                        && matches!(error, ActionError::UnsafePath(_) | ActionError::Conflict(_))
                    {
                        mark_action_operation_terminal(
                            &store,
                            &mut journal,
                            action_index,
                            OperationState::ManualRecoveryRequired,
                            OperationResultStatus::ManualRecoveryRequired,
                            error.to_string(),
                        )?;
                        journal.actions[action_index].state =
                            JournalActionState::ManualRecoveryRequired;
                        journal.actions[action_index].error = Some(error.to_string());
                        journal.cancellation.recovery_required = true;
                        store.persist(&mut journal)?;
                        return Err(ActionError::ManualRecoveryRequired(error.to_string()));
                    }
                    let deterministic = error.deterministic();
                    if deterministic {
                        mark_action_operation_terminal(
                            &store,
                            &mut journal,
                            action_index,
                            OperationState::FailedDeterministic,
                            OperationResultStatus::Failed,
                            error.to_string(),
                        )?;
                    }
                    record_action_failure(
                        &store,
                        &mut journal,
                        action_index,
                        error.to_string(),
                        deterministic,
                    )?;
                    if !deterministic {
                        return Err(ActionError::Interrupted(format!(
                            "action {} stopped in recoverable state: {error}",
                            action_index + 1
                        )));
                    }
                    if !journal.actions[action_index].continue_on_error {
                        mark_stop_after_failure(&store, &mut journal, action_index)?;
                        break;
                    }
                }
            }
        }

        let all_terminal = journal.actions.iter().all(|action| action.state.terminal());
        if !all_terminal {
            return Err(ActionError::Interrupted(
                "phase contains non-terminal journal state".to_string(),
            ));
        }
        // A recovered phase may carry the historical cancellation/recovery
        // marker that caused a previous runner to stop. Once every action has
        // reached a non-recovery terminal state, that marker is no longer an
        // accurate statement about the current durable result and would make
        // the election result contradict its action slots.
        let has_recovery_terminal = journal.actions.iter().any(|action| {
            matches!(
                action.state,
                JournalActionState::InterruptedRecoverable
                    | JournalActionState::ManualRecoveryRequired
            )
        });
        if !has_recovery_terminal
            && (journal.cancellation.recovery_required
                || (journal.cancellation.requested
                    && !journal.actions.iter().any(|action| {
                        action.state == JournalActionState::CancelledBeforeMutation
                    })))
        {
            journal.cancellation = CancellationRecord::default();
            store.persist(&mut journal)?;
        }
        cleanup_terminal_script_artifacts(
            self.scripts,
            self.filesystem,
            &store,
            &mut journal,
        )?;
        let cleanup_complete = cleanup_phase_artifacts(self.filesystem, &store, &mut journal)?;
        let mut report = report_from_journal(&journal)?;
        report.phase = Some(context.phase);
        finalize_terminal(
            &store,
            &mut journal,
            report.clone(),
            cleanup_complete,
        )?;
        prune_action_journal_retention_best_effort(context);
        Ok(report)
    }

    fn apply_action_operations(
        &self,
        store: &JournalStore<'_>,
        journal: &mut ActionJournal,
        action_index: usize,
        cancellation: &dyn ActionCancellation,
    ) -> Result<(), ActionError> {
        for operation_index in 0..journal.actions[action_index].operations.len() {
            if cancellation.is_cancelled() {
                let state = journal.actions[action_index].operations[operation_index].state;
                if !state.mutation_may_have_started()
                    && !journal.actions[action_index]
                        .root_materialization
                        .mutation_may_have_started()
                    && !journal.actions[action_index].operations[..operation_index]
                        .iter()
                        .any(|operation| operation.state.mutation_may_have_started())
                {
                    transition_operation(
                        store,
                        journal,
                        action_index,
                        operation_index,
                        OperationState::CancelledBeforeMutation,
                    )?;
                    return Err(ActionError::CancelledBeforeMutation(format!(
                        "action {} operation {}",
                        action_index + 1,
                        operation_index + 1
                    )));
                }
                journal.cancellation = CancellationRecord {
                    requested: true,
                    before_any_mutation: false,
                    recovery_required: true,
                };
                store.persist(journal)?;
                return Err(ActionError::Interrupted(format!(
                    "cancellation after action {} began",
                    action_index + 1
                )));
            }
            self.apply_operation(store, journal, action_index, operation_index, cancellation)?;
        }
        Ok(())
    }

    fn apply_operation(
        &self,
        store: &JournalStore<'_>,
        journal: &mut ActionJournal,
        action_index: usize,
        operation_index: usize,
        cancellation: &dyn ActionCancellation,
    ) -> Result<(), ActionError> {
        let plan = journal.actions[action_index].operations[operation_index]
            .plan
            .clone();
        match plan {
            PlannedOperation::Copy {
                source,
                destination,
                temporary,
                publication_witness,
                expected_source,
            } => self.apply_copy_like(
                store,
                journal,
                action_index,
                operation_index,
                CopyLikePaths {
                    source,
                    destination,
                    temporary,
                    publication_witness,
                    source_witness: None,
                    expected_source,
                },
            ),
            PlannedOperation::RepairCopyMetadata {
                source,
                destination,
                expected_source,
                expected_destination,
                include_hidden,
            } => self.apply_copy_metadata_repair(
                store,
                journal,
                action_index,
                operation_index,
                &source,
                &destination,
                &expected_source,
                &expected_destination,
                include_hidden,
            ),
            PlannedOperation::Move {
                source,
                destination,
                temporary,
                publication_witness,
                source_witness,
                expected_source,
            } => self.apply_copy_like(
                store,
                journal,
                action_index,
                operation_index,
                CopyLikePaths {
                    source,
                    destination,
                    temporary,
                    publication_witness,
                    source_witness: Some(source_witness),
                    expected_source,
                },
            ),
            PlannedOperation::Delete {
                target,
                witness,
                expected_target,
            } => self.apply_delete(
                store,
                journal,
                action_index,
                operation_index,
                &target,
                &witness,
                &expected_target,
            ),
            PlannedOperation::CreateDirectory { path } => self.apply_create_directory(
                store,
                journal,
                action_index,
                operation_index,
                &path,
            ),
            PlannedOperation::RunScript {
                script,
                expected_script,
                args,
                working_directory,
                environment,
                timeout_seconds,
                runtime_directory,
                containment_token,
            } => self.apply_script(
                store,
                journal,
                action_index,
                operation_index,
                ScriptInvocation {
                    script,
                    expected_script,
                    retained_script: None,
                    retained_working_directory: None,
                    args,
                    working_directory,
                    environment,
                    timeout: Duration::from_secs(timeout_seconds),
                    runtime_directory,
                    runtime_identity: None,
                    containment_token,
                },
                cancellation,
            ),
            PlannedOperation::Rename { .. } => Err(ActionError::InvalidJournal(
                "rename operation escaped action-level transaction executor".to_string(),
            )),
        }
    }
}

fn record_action_failure(
    store: &JournalStore<'_>,
    journal: &mut ActionJournal,
    action_index: usize,
    error: String,
    deterministic: bool,
) -> Result<(), ActionError> {
    journal.actions[action_index].error = Some(error);
    journal.actions[action_index].state = if deterministic {
        JournalActionState::FailedDeterministic
    } else {
        JournalActionState::InterruptedRecoverable
    };
    store.persist(journal)
}

fn mark_action_operation_terminal(
    store: &JournalStore<'_>,
    journal: &mut ActionJournal,
    action_index: usize,
    state: OperationState,
    status: OperationResultStatus,
    message: String,
) -> Result<(), ActionError> {
    let first_operation_index = journal.actions[action_index]
        .operations
        .iter()
        .position(|operation| !operation.state.terminal());
    let Some(first_operation_index) = first_operation_index else {
        return Ok(());
    };

    // Terminalize the failing operation and every later operation that has
    // not already reached a terminal state. Leaving a prepared tail behind a
    // terminal action is contradictory: after a crash it can look runnable
    // even though the durable action result says the action has stopped.
    for operation_index in first_operation_index..journal.actions[action_index].operations.len() {
        if journal.actions[action_index].operations[operation_index]
            .state
            .terminal()
        {
            continue;
        }
        transition_operation(store, journal, action_index, operation_index, state)?;
        let operation = &mut journal.actions[action_index].operations[operation_index];
        operation.result = Some(OperationResult {
            operation_id: operation.operation_id.clone(),
            summary: format!("{}: {message}", operation_summary(&operation.plan)),
            status,
            stdout_tail: None,
            stderr_tail: None,
        });
        store.persist(journal)?;
    }
    Ok(())
}

fn finalize_cancelled_before_mutation(
    store: &JournalStore<'_>,
    journal: &mut ActionJournal,
    action_index: usize,
    message: String,
) -> Result<(), ActionError> {
    mark_action_operation_terminal(
        store,
        journal,
        action_index,
        OperationState::CancelledBeforeMutation,
        OperationResultStatus::Skipped,
        message.clone(),
    )?;
    journal.cancellation = CancellationRecord {
        requested: true,
        before_any_mutation: !journal.actions[..action_index]
            .iter()
            .any(journal_action_mutation_may_have_started)
            && !journal.actions[action_index]
                .root_materialization
                .mutation_may_have_started(),
        recovery_required: false,
    };
    journal.actions[action_index].state = JournalActionState::CancelledBeforeMutation;
    journal.actions[action_index].error = Some(message);
    for later in journal.actions.iter_mut().skip(action_index + 1) {
        if !later.state.terminal() {
            later.state = JournalActionState::SkippedAfterFailure;
            later.error = Some("phase cancelled before mutation".to_string());
        }
    }
    store.persist(journal)?;
    let report = report_from_journal(journal)?;
    finalize_terminal(store, journal, report, true)
}

fn mark_stop_after_failure(
    store: &JournalStore<'_>,
    journal: &mut ActionJournal,
    failed_action_index: usize,
) -> Result<(), ActionError> {
    journal.stop_decision = Some(StopDecision {
        failed_action_index,
        remainder_marked_skipped: false,
    });
    for action in journal.actions.iter_mut().skip(failed_action_index + 1) {
        if !action.state.terminal() {
            action.state = JournalActionState::SkippedAfterFailure;
            action.error = Some(format!(
                "skipped because action {} failed with continue_on_error = false",
                failed_action_index + 1
            ));
        }
    }
    if let Some(stop) = journal.stop_decision.as_mut() {
        stop.remainder_marked_skipped = true;
    }
    store.persist(journal)
}

fn mark_remainder_skipped(
    store: &JournalStore<'_>,
    journal: &mut ActionJournal,
    start: usize,
) -> Result<(), ActionError> {
    let failed = journal
        .stop_decision
        .as_ref()
        .map(|decision| decision.failed_action_index)
        .unwrap_or(start.saturating_sub(1));
    for action in journal.actions.iter_mut().skip(start) {
        if !action.state.terminal() {
            action.state = JournalActionState::SkippedAfterFailure;
            action.error = Some(format!(
                "skipped because action {} stopped the phase",
                failed + 1
            ));
        }
    }
    if let Some(stop) = journal.stop_decision.as_mut() {
        stop.remainder_marked_skipped = true;
    }
    store.persist(journal)
}

fn register_journal_workspace_paths(
    filesystem: &dyn ActionFilesystem,
    journal: &mut ActionJournal,
    action_index: usize,
) -> Result<(), ActionError> {
    let mut new_paths = Vec::new();
    for operation in &journal.actions[action_index].operations {
        match &operation.plan {
            PlannedOperation::Rename { staging, .. } => {
                if let Some(parent) = staging.parent() {
                    new_paths.push(parent.to_path_buf());
                }
            }
            PlannedOperation::Copy {
                temporary,
                publication_witness,
                ..
            } => {
                new_paths.push(temporary.clone());
                new_paths.push(publication_witness.clone());
            }
            PlannedOperation::RepairCopyMetadata { .. } => {}
            PlannedOperation::Move {
                temporary,
                publication_witness,
                source_witness,
                ..
            } => {
                new_paths.push(temporary.clone());
                new_paths.push(publication_witness.clone());
                new_paths.push(source_witness.clone());
            }
            PlannedOperation::Delete { witness, .. } => new_paths.push(witness.clone()),
            PlannedOperation::CreateDirectory { .. } => {}
            PlannedOperation::RunScript { runtime_directory, .. } => {
                new_paths.push(runtime_directory.clone());
            }
        }
    }
    for path in new_paths {
        let scoped = filesystem.scoped_path(&path)?;
        if !journal.workspace_paths.contains(&path) {
            journal.workspace_paths.push(path);
            journal.workspace_capability_paths.push(scoped);
        }
    }
    Ok(())
}

fn transition_operation(
    store: &JournalStore<'_>,
    journal: &mut ActionJournal,
    action_index: usize,
    operation_index: usize,
    next: OperationState,
) -> Result<(), ActionError> {
    let operation = &journal.actions[action_index].operations[operation_index];
    if !legal_transition(operation.kind, operation.state, next) {
        return Err(ActionError::InvalidJournal(format!(
            "illegal {:?} transition {:?} -> {:?} for {}",
            operation.kind, operation.state, next, operation.operation_id
        )));
    }
    journal.actions[action_index].operations[operation_index].state = next;
    store.persist(journal)
}

fn legal_transition(kind: OperationKind, current: OperationState, next: OperationState) -> bool {
    if current == next {
        return true;
    }
    if matches!(next, OperationState::InterruptedRecoverable | OperationState::ManualRecoveryRequired)
    {
        return !current.terminal();
    }
    match kind {
        OperationKind::Copy => matches!(
            (current, next),
            (OperationState::Prepared, OperationState::CopyStarted)
                | (OperationState::CopyStarted, OperationState::CopyComplete)
                | (OperationState::CopyComplete, OperationState::Verified)
                | (OperationState::Verified, OperationState::PublishStarted)
                | (OperationState::PublishStarted, OperationState::Published)
                | (OperationState::Published, OperationState::Committed)
                | (OperationState::Committed, OperationState::CleanupStarted)
                | (OperationState::CleanupStarted, OperationState::CleanupComplete)
                | (OperationState::Prepared, OperationState::CancelledBeforeMutation)
                | (_, OperationState::FailedDeterministic)
        ),
        OperationKind::CopyMetadataRepair => matches!(
            (current, next),
            (OperationState::Prepared, OperationState::MetadataRepairStarted)
                | (OperationState::MetadataRepairStarted, OperationState::MetadataRepaired)
                | (OperationState::MetadataRepaired, OperationState::Committed)
                | (OperationState::Committed, OperationState::CleanupStarted)
                | (OperationState::CleanupStarted, OperationState::CleanupComplete)
                | (OperationState::Prepared, OperationState::CancelledBeforeMutation)
                | (_, OperationState::FailedDeterministic)
        ),
        OperationKind::Move => matches!(
            (current, next),
            (OperationState::Prepared, OperationState::DirectMoveStarted)
                | (OperationState::DirectMoveStarted, OperationState::DirectMoved)
                | (OperationState::DirectMoveStarted, OperationState::CopyStarted)
                | (OperationState::DirectMoved, OperationState::Committed)
                | (OperationState::CopyStarted, OperationState::CopyComplete)
                | (OperationState::CopyComplete, OperationState::Verified)
                | (OperationState::Verified, OperationState::PublishStarted)
                | (OperationState::PublishStarted, OperationState::Published)
                | (OperationState::Published, OperationState::SourceStageStarted)
                | (OperationState::SourceStageStarted, OperationState::SourceStaged)
                | (OperationState::SourceStaged, OperationState::DisposalStarted)
                | (OperationState::DisposalStarted, OperationState::Disposed)
                | (OperationState::Disposed, OperationState::Committed)
                | (OperationState::Committed, OperationState::CleanupStarted)
                | (OperationState::CleanupStarted, OperationState::CleanupComplete)
                | (OperationState::Prepared, OperationState::CancelledBeforeMutation)
                | (_, OperationState::FailedDeterministic)
        ),
        OperationKind::Delete => matches!(
            (current, next),
            (OperationState::Prepared, OperationState::SourceStageStarted)
                | (OperationState::SourceStageStarted, OperationState::SourceStaged)
                | (OperationState::SourceStaged, OperationState::DisposalStarted)
                | (OperationState::DisposalStarted, OperationState::Disposed)
                | (OperationState::Disposed, OperationState::Committed)
                | (OperationState::Committed, OperationState::CleanupStarted)
                | (OperationState::CleanupStarted, OperationState::CleanupComplete)
                | (OperationState::Prepared, OperationState::CancelledBeforeMutation)
                | (_, OperationState::FailedDeterministic)
        ),
        OperationKind::Rename => matches!(
            (current, next),
            (OperationState::Prepared, OperationState::RenameStageStarted)
                | (OperationState::RenameStageStarted, OperationState::RenameStaged)
                | (OperationState::RenameStaged, OperationState::RenamePublishStarted)
                | (OperationState::RenamePublishStarted, OperationState::RenamePublished)
                | (OperationState::RenamePublished, OperationState::Committed)
                | (OperationState::Committed, OperationState::CleanupStarted)
                | (OperationState::CleanupStarted, OperationState::CleanupComplete)
                | (OperationState::Prepared, OperationState::CancelledBeforeMutation)
                | (_, OperationState::FailedDeterministic)
        ),
        OperationKind::CreateDirectory => matches!(
            (current, next),
            (OperationState::Prepared, OperationState::DirectoryCreateStarted)
                | (OperationState::DirectoryCreateStarted, OperationState::Committed)
                | (OperationState::Committed, OperationState::CleanupStarted)
                | (OperationState::CleanupStarted, OperationState::CleanupComplete)
                | (OperationState::Prepared, OperationState::CancelledBeforeMutation)
                | (_, OperationState::FailedDeterministic)
        ),
        OperationKind::RunScript => matches!(
            (current, next),
            (OperationState::Prepared, OperationState::ScriptStartRecorded)
                | (OperationState::ScriptStartRecorded, OperationState::ScriptCompleted)
                | (OperationState::ScriptCompleted, OperationState::Committed)
                | (OperationState::Committed, OperationState::CleanupStarted)
                | (OperationState::CleanupStarted, OperationState::CleanupComplete)
                | (OperationState::Prepared, OperationState::CancelledBeforeMutation)
                | (OperationState::Prepared, OperationState::FailedDeterministic)
                | (OperationState::ScriptStartRecorded, OperationState::CancelledBeforeMutation)
                | (OperationState::ScriptStartRecorded, OperationState::FailedDeterministic)
                | (OperationState::ScriptStartRecorded, OperationState::ManualRecoveryRequired)
        ),
    }
}

struct CopyLikePaths {
    source: PathBuf,
    destination: PathBuf,
    temporary: PathBuf,
    publication_witness: PathBuf,
    source_witness: Option<PathBuf>,
    expected_source: ObjectIdentity,
}

impl<'a> ActionEngine<'a> {
    fn apply_copy_metadata_repair(
        &self,
        store: &JournalStore<'_>,
        journal: &mut ActionJournal,
        action_index: usize,
        operation_index: usize,
        source: &Path,
        destination: &Path,
        expected_source: &ObjectIdentity,
        expected_destination: &ObjectIdentity,
        include_hidden: bool,
    ) -> Result<(), ActionError> {
        loop {
            let state = journal.actions[action_index].operations[operation_index].state;
            match state {
                OperationState::Prepared => {
                    verify_same_copy_source(self.filesystem, source, expected_source)?;
                    let current_destination = self.filesystem.identity(destination, include_hidden)?;
                    if !current_destination.same_object(expected_destination) {
                        return Err(ActionError::PreviewStale(format!(
                            "copy metadata destination changed after planning: {}",
                            destination.display()
                        )));
                    }
                    if !current_destination.same_content(expected_source) {
                        return Err(ActionError::Contradiction(format!(
                            "copy metadata destination content no longer matches its source: {} -> {}",
                            source.display(),
                            destination.display()
                        )));
                    }
                    transition_operation(
                        store,
                        journal,
                        action_index,
                        operation_index,
                        OperationState::MetadataRepairStarted,
                    )?;
                }
                OperationState::MetadataRepairStarted => {
                    verify_same_copy_source(self.filesystem, source, expected_source)?;
                    let current_destination = self.filesystem.identity(destination, include_hidden)?;
                    verify_same_filesystem_object_authority(
                        &current_destination,
                        expected_destination,
                        destination,
                    )?;
                    if !current_destination.same_content(expected_source) {
                        return Err(ActionError::Contradiction(format!(
                            "copy metadata repair found changed destination content: {}",
                            destination.display()
                        )));
                    }
                    self.filesystem
                        .repair_copy_metadata(source, destination, include_hidden)?;
                    let repaired = self.filesystem.identity(destination, include_hidden)?;
                    if !repaired.copy_state_equivalent(expected_source) {
                        return Err(ActionError::Contradiction(format!(
                            "copy metadata repair did not establish canonical copy state: {}",
                            destination.display()
                        )));
                    }
                    journal.actions[action_index].operations[operation_index]
                        .observed_destination = Some(repaired);
                    transition_operation(
                        store,
                        journal,
                        action_index,
                        operation_index,
                        OperationState::MetadataRepaired,
                    )?;
                }
                OperationState::MetadataRepaired => {
                    verify_same_copy_source(self.filesystem, source, expected_source)?;
                    let repaired = self.filesystem.identity(destination, include_hidden)?;
                    if !repaired.copy_state_equivalent(expected_source) {
                        return Err(ActionError::Contradiction(format!(
                            "copy metadata destination changed after repair: {}",
                            destination.display()
                        )));
                    }
                    transition_operation(
                        store,
                        journal,
                        action_index,
                        operation_index,
                        OperationState::Committed,
                    )?;
                }
                OperationState::Committed => {
                    transition_operation(
                        store,
                        journal,
                        action_index,
                        operation_index,
                        OperationState::CleanupStarted,
                    )?;
                }
                OperationState::CleanupStarted => {
                    // Result must be durable IN the CleanupComplete persist:
                    // a crash between the transition and a later result write
                    // leaves a clean operation without a result, which
                    // recovery rejects as an invalid journal.
                    set_completed_operation_result(journal, action_index, operation_index);
                    transition_operation(
                        store,
                        journal,
                        action_index,
                        operation_index,
                        OperationState::CleanupComplete,
                    )?;
                    return Ok(());
                }
                OperationState::CleanupComplete => {
                    set_completed_operation_result(journal, action_index, operation_index);
                    return Ok(());
                }
                OperationState::CancelledBeforeMutation => {
                    return Err(ActionError::CancelledBeforeMutation(format!(
                        "copy metadata repair cancelled before mutation: {}",
                        destination.display()
                    )));
                }
                OperationState::FailedDeterministic => {
                    return Err(ActionError::Conflict(format!(
                        "copy metadata repair previously failed deterministically: {}",
                        destination.display()
                    )));
                }
                OperationState::ManualRecoveryRequired => {
                    return Err(ActionError::ManualRecoveryRequired(format!(
                        "copy metadata repair requires manual recovery: {}",
                        destination.display()
                    )));
                }
                other => {
                    return Err(ActionError::InvalidJournal(format!(
                        "copy metadata repair entered impossible state {other:?}"
                    )));
                }
            }
        }
    }

    fn apply_copy_like(
        &self,
        store: &JournalStore<'_>,
        journal: &mut ActionJournal,
        action_index: usize,
        operation_index: usize,
        paths: CopyLikePaths,
    ) -> Result<(), ActionError> {
        let prepublication_witness = prepublication_witness_path(
            &paths.publication_witness,
            &paths.temporary,
            &paths.expected_source,
        )?;
        loop {
            let state = journal.actions[action_index].operations[operation_index].state;
            match state {
                OperationState::Prepared => {
                    verify_same_copy_source(self.filesystem, &paths.source, &paths.expected_source)?;
                    assert_path_absent(self.filesystem, &paths.temporary, "copy temporary")?;
                    assert_path_absent(
                        self.filesystem,
                        &paths.publication_witness,
                        "publication witness",
                    )?;
                    if prepublication_witness != paths.publication_witness {
                        assert_path_absent(
                            self.filesystem,
                            &prepublication_witness,
                            "prepublication witness",
                        )?;
                    }
                    if let Some(source_witness) = &paths.source_witness {
                        assert_path_absent(
                            self.filesystem,
                            source_witness,
                            "move source witness",
                        )?;
                    }
                    if self.filesystem.path_exists_no_follow(&paths.destination)? {
                        return Err(ActionError::Contradiction(format!(
                            "destination appeared after planning: {}",
                            paths.destination.display()
                        )));
                    }
                    transition_operation(
                        store,
                        journal,
                        action_index,
                        operation_index,
                        if paths.source_witness.is_some() {
                            OperationState::DirectMoveStarted
                        } else {
                            OperationState::CopyStarted
                        },
                    )?;
                }
                OperationState::DirectMoveStarted => {
                    if paths.source_witness.is_none() {
                        return Err(ActionError::InvalidJournal(
                            "copy operation entered direct-move state".to_string(),
                        ));
                    }
                    let source_exists = self.filesystem.path_exists_no_follow(&paths.source)?;
                    let destination_exists =
                        self.filesystem.path_exists_no_follow(&paths.destination)?;
                    match (source_exists, destination_exists) {
                        (true, false) => {
                            verify_same_source(
                                self.filesystem,
                                &paths.source,
                                &paths.expected_source,
                            )?;
                            match self.filesystem.try_move_no_clobber(
                                &paths.source,
                                &paths.destination,
                                &paths.expected_source,
                            )? {
                                MoveRenameAttempt::Renamed => {
                                    let observed = self.filesystem.identity(
                                        &paths.destination,
                                        identity_includes_hidden(
                                            self.filesystem,
                                            &paths.expected_source,
                                            &paths.destination,
                                        )?,
                                    )?;
                                    if !observed.copy_state_equivalent(&paths.expected_source) {
                                        return Err(ActionError::Contradiction(format!(
                                            "direct move published content with an unexpected identity: {}",
                                            paths.destination.display()
                                        )));
                                    }
                                    verify_same_filesystem_object_authority(
                                        &observed,
                                        &paths.expected_source,
                                        &paths.destination,
                                    )?;
                                    journal.actions[action_index].operations[operation_index]
                                        .observed_destination = Some(observed);
                                    transition_operation(
                                        store,
                                        journal,
                                        action_index,
                                        operation_index,
                                        OperationState::DirectMoved,
                                    )?;
                                }
                                MoveRenameAttempt::CrossDevice => {
                                    transition_operation(
                                        store,
                                        journal,
                                        action_index,
                                        operation_index,
                                        OperationState::CopyStarted,
                                    )?;
                                }
                            }
                        }
                        (false, true) => {
                            let observed = self.filesystem.identity(
                                &paths.destination,
                                identity_includes_hidden(
                                    self.filesystem,
                                    &paths.expected_source,
                                    &paths.destination,
                                )?,
                            )?;
                            if !observed.copy_state_equivalent(&paths.expected_source) {
                                return Err(ActionError::Contradiction(format!(
                                    "direct-move recovery found destination content from an unrelated object: {}",
                                    paths.destination.display()
                                )));
                            }
                            verify_same_filesystem_object_authority(
                                &observed,
                                &paths.expected_source,
                                &paths.destination,
                            )?;
                            journal.actions[action_index].operations[operation_index]
                                .observed_destination = Some(observed);
                            transition_operation(
                                store,
                                journal,
                                action_index,
                                operation_index,
                                OperationState::DirectMoved,
                            )?;
                        }
                        (true, true) => {
                            return Err(ActionError::Contradiction(format!(
                                "direct-move recovery found both source and destination: {} -> {}",
                                paths.source.display(),
                                paths.destination.display()
                            )));
                        }
                        (false, false) => {
                            return Err(ActionError::Contradiction(format!(
                                "direct-move recovery found neither source nor destination: {} -> {}",
                                paths.source.display(),
                                paths.destination.display()
                            )));
                        }
                    }
                }
                OperationState::DirectMoved => {
                    if let Some(source_witness) = &paths.source_witness {
                        assert_path_absent(
                            self.filesystem,
                            source_witness,
                            "unused direct-move witness",
                        )?;
                    }
                    if self.filesystem.path_exists_no_follow(&paths.source)? {
                        return Err(ActionError::Contradiction(format!(
                            "direct-move source pathname was recreated: {}",
                            paths.source.display()
                        )));
                    }
                    verify_observed_destination(
                        self.filesystem,
                        &paths.destination,
                        &paths.expected_source,
                        journal.actions[action_index].operations[operation_index]
                            .observed_destination
                            .as_ref(),
                    )?;
                    transition_operation(
                        store,
                        journal,
                        action_index,
                        operation_index,
                        OperationState::Committed,
                    )?;
                }
                OperationState::CopyStarted => {
                    verify_same_copy_source(self.filesystem, &paths.source, &paths.expected_source)?;
                    if self.filesystem.path_exists_no_follow(&paths.destination)? {
                        return Err(ActionError::Contradiction(format!(
                            "destination exists before publish was authorized: {}",
                            paths.destination.display()
                        )));
                    }
                    if self
                        .filesystem
                        .path_exists_no_follow(&paths.publication_witness)?
                    {
                        return Err(ActionError::Contradiction(format!(
                            "publication witness exists before publish state: {}",
                            paths.publication_witness.display()
                        )));
                    }
                    if prepublication_witness != paths.publication_witness
                        && self
                            .filesystem
                            .path_exists_no_follow(&prepublication_witness)?
                    {
                        return Err(ActionError::Contradiction(format!(
                            "prepublication witness exists before publish state: {}",
                            prepublication_witness.display()
                        )));
                    }
                    // A crash during copy may leave a partial journal-owned
                    // temporary. It is safe to discard only because the full
                    // path is recorded in the validated journal and publish has
                    // not started.
                    if let Some(temporary_identity) =
                        self.filesystem.entry_identity(&paths.temporary)?
                    {
                        self.filesystem
                            .remove_owned_path(&paths.temporary, temporary_identity)?;
                    }
                    self.filesystem.copy_to_temporary(
                        &paths.source,
                        &paths.temporary,
                        identity_includes_hidden(self.filesystem, &paths.expected_source, &paths.source)?,
                    )?;
                    transition_operation(
                        store,
                        journal,
                        action_index,
                        operation_index,
                        OperationState::CopyComplete,
                    )?;
                }
                OperationState::CopyComplete => {
                    verify_copy_state_equivalent(
                        self.filesystem,
                        &paths.temporary,
                        &paths.expected_source,
                    )?;
                    transition_operation(
                        store,
                        journal,
                        action_index,
                        operation_index,
                        OperationState::Verified,
                    )?;
                }
                OperationState::Verified => {
                    verify_same_copy_source(self.filesystem, &paths.source, &paths.expected_source)?;
                    verify_copy_state_equivalent(
                        self.filesystem,
                        &paths.temporary,
                        &paths.expected_source,
                    )?;
                    if self.filesystem.path_exists_no_follow(&paths.destination)? {
                        return Err(ActionError::Contradiction(format!(
                            "destination appeared after verification: {}",
                            paths.destination.display()
                        )));
                    }
                    transition_operation(
                        store,
                        journal,
                        action_index,
                        operation_index,
                        OperationState::PublishStarted,
                    )?;
                    write_publication_witness(
                        self.filesystem,
                        &prepublication_witness,
                        &paths.temporary,
                        &journal.claim_id,
                        &journal.actions[action_index].operations[operation_index].operation_id,
                        &paths.expected_source,
                    )?;
                    self.filesystem
                        .publish_no_clobber(&paths.temporary, &paths.destination)?;
                    let observed = verify_published_destination(
                        self.filesystem,
                        &paths.destination,
                        &paths.temporary,
                        &paths.publication_witness,
                        &paths.expected_source,
                        None,
                    )?;
                    journal.actions[action_index].operations[operation_index]
                        .observed_destination = Some(observed);
                    transition_operation(
                        store,
                        journal,
                        action_index,
                        operation_index,
                        OperationState::Published,
                    )?;
                }
                OperationState::PublishStarted => {
                    let final_witness_exists = self
                        .filesystem
                        .path_exists_no_follow(&paths.publication_witness)?;
                    let pre_witness_exists = self
                        .filesystem
                        .path_exists_no_follow(&prepublication_witness)?;
                    let destination_exists = self
                        .filesystem
                        .path_exists_no_follow(&paths.destination)?;

                    if destination_exists {
                        if !final_witness_exists {
                            return Err(ActionError::Contradiction(format!(
                                "destination exists but its protected publication witness is absent: {}",
                                paths.destination.display()
                            )));
                        }
                        validate_publication_witness(
                            self.filesystem,
                            &paths.publication_witness,
                            &journal.claim_id,
                            &journal.actions[action_index].operations[operation_index]
                                .operation_id,
                            &paths.expected_source,
                        )?;
                    } else {
                        verify_content_excluding_publication_witness(
                            self.filesystem,
                            &paths.temporary,
                            &prepublication_witness,
                            &paths.expected_source,
                        )?;
                        if pre_witness_exists {
                            validate_publication_witness(
                                self.filesystem,
                                &prepublication_witness,
                                &journal.claim_id,
                                &journal.actions[action_index].operations[operation_index]
                                    .operation_id,
                                &paths.expected_source,
                            )?;
                        } else {
                            write_publication_witness(
                                self.filesystem,
                                &prepublication_witness,
                                &paths.temporary,
                                &journal.claim_id,
                                &journal.actions[action_index].operations[operation_index]
                                    .operation_id,
                                &paths.expected_source,
                            )?;
                        }
                    }

                    if destination_exists {
                        if prepublication_witness != paths.publication_witness
                            && pre_witness_exists
                        {
                            return Err(ActionError::Contradiction(format!(
                                "directory publication witness exists in both staged and published trees: {}",
                                prepublication_witness.display()
                            )));
                        }
                        let observed = verify_published_destination(
                            self.filesystem,
                            &paths.destination,
                            &paths.temporary,
                            &paths.publication_witness,
                            &paths.expected_source,
                            None,
                        )?;
                        journal.actions[action_index].operations[operation_index]
                            .observed_destination = Some(observed);
                    } else {
                        verify_content_excluding_publication_witness(
                            self.filesystem,
                            &paths.temporary,
                            &prepublication_witness,
                            &paths.expected_source,
                        )?;
                        self.filesystem
                            .publish_no_clobber(&paths.temporary, &paths.destination)?;
                        let observed = verify_published_destination(
                            self.filesystem,
                            &paths.destination,
                            &paths.temporary,
                            &paths.publication_witness,
                            &paths.expected_source,
                            None,
                        )?;
                        journal.actions[action_index].operations[operation_index]
                            .observed_destination = Some(observed);
                    }
                    transition_operation(
                        store,
                        journal,
                        action_index,
                        operation_index,
                        OperationState::Published,
                    )?;
                }
                OperationState::Published => {
                    verify_published_destination(
                        self.filesystem,
                        &paths.destination,
                        &paths.temporary,
                        &paths.publication_witness,
                        &paths.expected_source,
                        journal.actions[action_index].operations[operation_index]
                            .observed_destination
                            .as_ref(),
                    )?;
                    if paths.source_witness.is_some() {
                        transition_operation(
                            store,
                            journal,
                            action_index,
                            operation_index,
                            OperationState::SourceStageStarted,
                        )?;
                    } else {
                        transition_operation(
                            store,
                            journal,
                            action_index,
                            operation_index,
                            OperationState::Committed,
                        )?;
                    }
                }
                OperationState::SourceStageStarted => {
                    let witness = paths.source_witness.as_ref().ok_or_else(|| {
                        ActionError::InvalidJournal(
                            "copy operation entered move source-removal state".to_string(),
                        )
                    })?;
                    verify_published_destination(
                        self.filesystem,
                        &paths.destination,
                        &paths.temporary,
                        &paths.publication_witness,
                        &paths.expected_source,
                        journal.actions[action_index].operations[operation_index]
                            .observed_destination
                            .as_ref(),
                    )?;
                    reconcile_stage_to_witness(
                        self.filesystem,
                        &paths.source,
                        witness,
                        &paths.expected_source,
                        "move source",
                    )?;
                    transition_operation(
                        store,
                        journal,
                        action_index,
                        operation_index,
                        OperationState::SourceStaged,
                    )?;
                }
                OperationState::SourceStaged => {
                    let witness = paths.source_witness.as_ref().ok_or_else(|| {
                        ActionError::InvalidJournal(
                            "copy operation entered move source-removal state".to_string(),
                        )
                    })?;
                    ensure_original_absent_and_witness_matches(
                        self.filesystem,
                        &paths.source,
                        witness,
                        &paths.expected_source,
                        "move source",
                    )?;
                    verify_published_destination(
                        self.filesystem,
                        &paths.destination,
                        &paths.temporary,
                        &paths.publication_witness,
                        &paths.expected_source,
                        journal.actions[action_index].operations[operation_index]
                            .observed_destination
                            .as_ref(),
                    )?;
                    transition_operation(
                        store,
                        journal,
                        action_index,
                        operation_index,
                        OperationState::DisposalStarted,
                    )?;
                }
                OperationState::DisposalStarted => {
                    let witness = paths.source_witness.as_ref().ok_or_else(|| {
                        ActionError::InvalidJournal(
                            "copy operation entered move disposal state".to_string(),
                        )
                    })?;
                    if self.filesystem.path_exists_no_follow(&paths.source)? {
                        return Err(ActionError::Contradiction(format!(
                            "move source pathname was recreated after staging: {}",
                            paths.source.display()
                        )));
                    }
                    verify_published_destination(
                        self.filesystem,
                        &paths.destination,
                        &paths.temporary,
                        &paths.publication_witness,
                        &paths.expected_source,
                        journal.actions[action_index].operations[operation_index]
                            .observed_destination
                            .as_ref(),
                    )?;
                    if self.filesystem.path_exists_no_follow(witness)? {
                        verify_relocated_source(self.filesystem, witness, &paths.expected_source)?;
                        self.filesystem.remove_owned_path(
                            witness,
                            cap_entry_identity(&paths.expected_source)?,
                        )?;
                    }
                    transition_operation(
                        store,
                        journal,
                        action_index,
                        operation_index,
                        OperationState::Disposed,
                    )?;
                }
                OperationState::Disposed => {
                    if self.filesystem.path_exists_no_follow(&paths.source)? {
                        return Err(ActionError::Contradiction(format!(
                            "move source pathname was recreated: {}",
                            paths.source.display()
                        )));
                    }
                    if let Some(witness) = &paths.source_witness {
                        if self.filesystem.path_exists_no_follow(witness)? {
                            return Err(ActionError::Contradiction(format!(
                                "move source witness survived disposal: {}",
                                witness.display()
                            )));
                        }
                    }
                    verify_published_destination(
                        self.filesystem,
                        &paths.destination,
                        &paths.temporary,
                        &paths.publication_witness,
                        &paths.expected_source,
                        journal.actions[action_index].operations[operation_index]
                            .observed_destination
                            .as_ref(),
                    )?;
                    transition_operation(
                        store,
                        journal,
                        action_index,
                        operation_index,
                        OperationState::Committed,
                    )?;
                }
                OperationState::Committed => {
                    // A move's source was removed before Committed was
                    // persisted; a present source is a post-crash recreation
                    // and recovery must fail closed rather than adopt it
                    // (same rule as the DirectMoved arm).
                    if paths.source_witness.is_some()
                        && self.filesystem.path_exists_no_follow(&paths.source)?
                    {
                        return Err(ActionError::Contradiction(format!(
                            "move source pathname was recreated after commit: {}",
                            paths.source.display()
                        )));
                    }
                    transition_operation(
                        store,
                        journal,
                        action_index,
                        operation_index,
                        OperationState::CleanupStarted,
                    )?;
                }
                OperationState::CleanupStarted => {
                    if self
                        .filesystem
                        .path_exists_no_follow(&paths.publication_witness)?
                    {
                        verify_published_destination(
                            self.filesystem,
                            &paths.destination,
                            &paths.temporary,
                            &paths.publication_witness,
                            &paths.expected_source,
                            journal.actions[action_index].operations[operation_index]
                                .observed_destination
                                .as_ref(),
                        )?;
                    } else {
                        // CleanupStarted is durable before removing either
                        // witness. A crash after witness removal resumes here;
                        // the observed published-object identity remains the
                        // authority and prevents accepting a replacement.
                        verify_observed_destination(
                            self.filesystem,
                            &paths.destination,
                            &paths.expected_source,
                            journal.actions[action_index].operations[operation_index]
                                .observed_destination
                                .as_ref(),
                        )?;
                    }
                    if self.filesystem.path_exists_no_follow(&paths.temporary)? {
                        let observed = journal.actions[action_index].operations[operation_index]
                            .observed_destination
                            .as_ref()
                            .ok_or_else(|| ActionError::InvalidJournal(
                                "published operation is missing its observed destination identity"
                                    .to_string(),
                            ))?;
                        self.filesystem.remove_owned_path(
                            &paths.temporary,
                            cap_entry_identity(observed)?,
                        )?;
                    }
                    if self
                        .filesystem
                        .path_exists_no_follow(&paths.publication_witness)?
                    {
                        let witness_identity = validate_publication_witness(
                            self.filesystem,
                            &paths.publication_witness,
                            &journal.claim_id,
                            &journal.actions[action_index].operations[operation_index]
                                .operation_id,
                            &paths.expected_source,
                        )?;
                        self.filesystem.remove_owned_path(
                            &paths.publication_witness,
                            witness_identity,
                        )?;
                    }
                    // Result must be durable IN the CleanupComplete persist:
                    // a crash between the transition and a later result write
                    // leaves a clean operation without a result, which
                    // recovery rejects as an invalid journal.
                    set_completed_operation_result(journal, action_index, operation_index);
                    transition_operation(
                        store,
                        journal,
                        action_index,
                        operation_index,
                        OperationState::CleanupComplete,
                    )?;
                }
                OperationState::CleanupComplete => return Ok(()),
                OperationState::CancelledBeforeMutation => {
                    return Err(ActionError::CancelledBeforeMutation(format!(
                        "operation {} was durably cancelled before mutation",
                        journal.actions[action_index].operations[operation_index].operation_id
                    )));
                }
                OperationState::FailedDeterministic => {
                    return Err(ActionError::Conflict(
                        journal.actions[action_index].operations[operation_index]
                            .result
                            .as_ref()
                            .map(|result| result.summary.clone())
                            .unwrap_or_else(|| "operation previously failed".to_string()),
                    ));
                }
                OperationState::ManualRecoveryRequired
                | OperationState::ScriptStartRecorded => {
                    return Err(ActionError::ManualRecoveryRequired(format!(
                        "operation {} cannot be replayed automatically",
                        journal.actions[action_index].operations[operation_index].operation_id
                    )));
                }
                other => {
                    return Err(ActionError::InvalidJournal(format!(
                        "copy/move operation has impossible state {other:?}"
                    )));
                }
            }
        }
    }

    fn apply_delete(
        &self,
        store: &JournalStore<'_>,
        journal: &mut ActionJournal,
        action_index: usize,
        operation_index: usize,
        target: &Path,
        witness: &Path,
        expected: &ObjectIdentity,
    ) -> Result<(), ActionError> {
        loop {
            match journal.actions[action_index].operations[operation_index].state {
                OperationState::Prepared => {
                    verify_same_source(self.filesystem, target, expected)?;
                    assert_path_absent(self.filesystem, witness, "delete witness")?;
                    transition_operation(
                        store,
                        journal,
                        action_index,
                        operation_index,
                        OperationState::SourceStageStarted,
                    )?;
                }
                OperationState::SourceStageStarted => {
                    reconcile_stage_to_witness(
                        self.filesystem,
                        target,
                        witness,
                        expected,
                        "delete target",
                    )?;
                    transition_operation(
                        store,
                        journal,
                        action_index,
                        operation_index,
                        OperationState::SourceStaged,
                    )?;
                }
                OperationState::SourceStaged => {
                    ensure_original_absent_and_witness_matches(
                        self.filesystem,
                        target,
                        witness,
                        expected,
                        "delete target",
                    )?;
                    transition_operation(
                        store,
                        journal,
                        action_index,
                        operation_index,
                        OperationState::DisposalStarted,
                    )?;
                }
                OperationState::DisposalStarted => {
                    if self.filesystem.path_exists_no_follow(target)? {
                        return Err(ActionError::Contradiction(format!(
                            "delete pathname was recreated after staging: {}",
                            target.display()
                        )));
                    }
                    if self.filesystem.path_exists_no_follow(witness)? {
                        verify_relocated_source(self.filesystem, witness, expected)?;
                        self.filesystem
                            .remove_owned_path(witness, cap_entry_identity(expected)?)?;
                    }
                    transition_operation(
                        store,
                        journal,
                        action_index,
                        operation_index,
                        OperationState::Disposed,
                    )?;
                }
                OperationState::Disposed => {
                    if self.filesystem.path_exists_no_follow(target)? {
                        return Err(ActionError::Contradiction(format!(
                            "delete pathname was recreated: {}",
                            target.display()
                        )));
                    }
                    if self.filesystem.path_exists_no_follow(witness)? {
                        return Err(ActionError::Contradiction(format!(
                            "delete witness survived disposal: {}",
                            witness.display()
                        )));
                    }
                    transition_operation(
                        store,
                        journal,
                        action_index,
                        operation_index,
                        OperationState::Committed,
                    )?;
                }
                OperationState::Committed => transition_operation(
                    store,
                    journal,
                    action_index,
                    operation_index,
                    OperationState::CleanupStarted,
                )?,
                OperationState::CleanupStarted => {
                    // Result must be durable IN the CleanupComplete persist:
                    // a crash between the transition and a later result write
                    // leaves a clean operation without a result, which
                    // recovery rejects as an invalid journal.
                    set_completed_operation_result(journal, action_index, operation_index);
                    transition_operation(
                        store,
                        journal,
                        action_index,
                        operation_index,
                        OperationState::CleanupComplete,
                    )?;
                }
                OperationState::CleanupComplete => return Ok(()),
                OperationState::CancelledBeforeMutation => {
                    return Err(ActionError::CancelledBeforeMutation(format!(
                        "delete operation {} was cancelled before mutation",
                        journal.actions[action_index].operations[operation_index].operation_id
                    )));
                }
                other => {
                    return Err(ActionError::InvalidJournal(format!(
                        "delete operation has impossible state {other:?}"
                    )));
                }
            }
        }
    }

    fn apply_create_directory(
        &self,
        store: &JournalStore<'_>,
        journal: &mut ActionJournal,
        action_index: usize,
        operation_index: usize,
        path: &Path,
    ) -> Result<(), ActionError> {
        loop {
            match journal.actions[action_index].operations[operation_index].state {
                OperationState::Prepared => transition_operation(
                    store,
                    journal,
                    action_index,
                    operation_index,
                    OperationState::DirectoryCreateStarted,
                )?,
                OperationState::DirectoryCreateStarted => {
                    if self.filesystem.path_exists_no_follow(path)? {
                        if self.filesystem.identity(path, true)?.kind != ObjectKind::Directory {
                            return Err(ActionError::Contradiction(format!(
                                "create_folder target became a non-directory: {}",
                                path.display()
                            )));
                        }
                    } else {
                        self.filesystem.create_dir_all(path)?;
                    }
                    transition_operation(
                        store,
                        journal,
                        action_index,
                        operation_index,
                        OperationState::Committed,
                    )?;
                }
                OperationState::Committed => transition_operation(
                    store,
                    journal,
                    action_index,
                    operation_index,
                    OperationState::CleanupStarted,
                )?,
                OperationState::CleanupStarted => {
                    // Result must be durable IN the CleanupComplete persist:
                    // a crash between the transition and a later result write
                    // leaves a clean operation without a result, which
                    // recovery rejects as an invalid journal.
                    set_completed_operation_result(journal, action_index, operation_index);
                    transition_operation(
                        store,
                        journal,
                        action_index,
                        operation_index,
                        OperationState::CleanupComplete,
                    )?;
                }
                OperationState::CleanupComplete => return Ok(()),
                other => {
                    return Err(ActionError::InvalidJournal(format!(
                        "create_folder operation has impossible state {other:?}"
                    )));
                }
            }
        }
    }

    fn apply_script(
        &self,
        store: &JournalStore<'_>,
        journal: &mut ActionJournal,
        action_index: usize,
        operation_index: usize,
        mut invocation: ScriptInvocation,
        cancellation: &dyn ActionCancellation,
    ) -> Result<(), ActionError> {
        let state = journal.actions[action_index].operations[operation_index].state;
        match state {
            OperationState::Prepared => {
                if cancellation.is_cancelled() {
                    set_script_terminal(
                        journal,
                        action_index,
                        operation_index,
                        ScriptTerminalState::SetupFailedBeforeExecution,
                    )?;
                    transition_operation(
                        store,
                        journal,
                        action_index,
                        operation_index,
                        OperationState::CancelledBeforeMutation,
                    )?;
                    return Err(ActionError::CancelledBeforeMutation(
                        "script was not started".to_string(),
                    ));
                }
                validate_script_execution_plan(
                    journal,
                    action_index,
                    operation_index,
                    &invocation,
                )?;
                match self.filesystem.open_verified_regular(
                    &invocation.script,
                    &invocation.expected_script,
                ) {
                    Ok(file) => invocation.retained_script = Some(Arc::new(file)),
                    Err(error) => {
                        let summary = format!(
                            "reviewed script changed before retained-descriptor execution: {} ({error})",
                            invocation.script.display()
                        );
                        set_script_terminal(
                            journal,
                            action_index,
                            operation_index,
                            ScriptTerminalState::SetupFailedBeforeExecution,
                        )?;
                        set_script_result(
                            journal,
                            action_index,
                            operation_index,
                            summary.clone(),
                            OperationResultStatus::Failed,
                            None,
                            None,
                        );
                        return Err(ActionError::Conflict(summary));
                    }
                }
                match self
                    .filesystem
                    .open_directory_handle(&invocation.working_directory)
                {
                    Ok(directory) => {
                        invocation.retained_working_directory = Some(Arc::new(directory));
                    }
                    Err(error) => {
                        let summary = format!(
                            "script working directory changed before retained-descriptor execution: {} ({error})",
                            invocation.working_directory.display()
                        );
                        set_script_terminal(
                            journal,
                            action_index,
                            operation_index,
                            ScriptTerminalState::SetupFailedBeforeExecution,
                        )?;
                        set_script_result(
                            journal,
                            action_index,
                            operation_index,
                            summary.clone(),
                            OperationResultStatus::Failed,
                            None,
                            None,
                        );
                        return Err(ActionError::Conflict(summary));
                    }
                }
                if self
                    .filesystem
                    .path_exists_no_follow(&invocation.runtime_directory)?
                {
                    let recorded_identity = script_execution(
                        journal,
                        action_index,
                        operation_index,
                    )?
                    .runtime_identity;
                    if let Some(recorded_identity) = recorded_identity {
                        let expected = CapEntryIdentity {
                            file_type: CapFileType::Directory,
                            device: recorded_identity.device,
                            inode: recorded_identity.inode,
                        };
                        self.filesystem.remove_owned_path(
                            &invocation.runtime_directory,
                            expected,
                        )?;
                        self.filesystem.sync_parent(&invocation.runtime_directory)?;
                        script_execution_mut(
                            journal,
                            action_index,
                            operation_index,
                        )?
                        .runtime_identity = None;
                        store.persist(journal)?;
                    } else {
                        let summary = format!(
                            "script runtime directory already exists without durable ownership identity: {}; automatic replay is refused",
                            invocation.runtime_directory.display()
                        );
                        set_script_terminal(
                            journal,
                            action_index,
                            operation_index,
                            ScriptTerminalState::ContainmentUncertain,
                        )?;
                        set_script_result(
                            journal,
                            action_index,
                            operation_index,
                            summary.clone(),
                            OperationResultStatus::ManualRecoveryRequired,
                            None,
                            None,
                        );
                        transition_operation(
                            store,
                            journal,
                            action_index,
                            operation_index,
                            OperationState::ManualRecoveryRequired,
                        )?;
                        return Err(ActionError::ManualRecoveryRequired(summary));
                    }
                }
                self.filesystem
                    .create_private_dir_all(&invocation.runtime_directory)?;
                let runtime_identity = self
                    .filesystem
                    .entry_identity(&invocation.runtime_directory)?
                    .ok_or_else(|| {
                        ActionError::Contradiction(
                            "new script runtime directory vanished before supervisor launch"
                                .to_string(),
                        )
                    })?;
                if runtime_identity.file_type != CapFileType::Directory {
                    return Err(ActionError::Contradiction(
                        "new script runtime path is not a directory".to_string(),
                    ));
                }
                let runtime_identity = RuntimeDirectoryIdentity {
                    device: runtime_identity.device,
                    inode: runtime_identity.inode,
                };
                invocation.runtime_identity = Some(runtime_identity);
                script_execution_mut(journal, action_index, operation_index)?
                    .runtime_identity = Some(runtime_identity);
                store.persist(journal)?;

                let mut observer = |event: &ScriptLifecycleEvent| {
                    record_script_lifecycle_event(
                        store,
                        journal,
                        action_index,
                        operation_index,
                        event,
                    )
                };
                let run_result = self
                    .scripts
                    .run(&invocation, cancellation, &mut observer);
                drop(observer);

                let outcome = match run_result {
                    Ok(outcome) => outcome,
                    Err(error) => {
                        let started = script_execution(journal, action_index, operation_index)?
                            .start_committed;
                        if started {
                            // A failed live supervisor must not merely return
                            // while its backend-owned domain may still be
                            // running.  Use the same stable recovery authority
                            // immediately; a cgroup can be terminated even if
                            // the helper itself died, while weaker backends
                            // fail loudly when their supervisor/result is no
                            // longer authoritative.
                            let record = script_execution(
                                journal,
                                action_index,
                                operation_index,
                            )?
                            .clone();
                            let recovery = if let Some(descriptor) = record.descriptor.clone() {
                                let request = ScriptRecoveryRequest {
                                    token: record.token.clone(),
                                    runtime_directory: record.runtime_directory.clone(),
                                    descriptor: descriptor.clone(),
                                };
                                let mut recovery_observer = |event: &ScriptLifecycleEvent| {
                                    record_script_recovery_lifecycle_event(
                                        store,
                                        journal,
                                        action_index,
                                        operation_index,
                                        event,
                                    )
                                };
                                let recovered = self
                                    .scripts
                                    .recover(&request, &mut recovery_observer);
                                drop(recovery_observer);
                                if matches!(
                                    &recovered,
                                    Ok(ScriptRecoveryOutcome::ExecutionNeverReleased)
                                        | Ok(ScriptRecoveryOutcome::ContainmentAlreadyEmpty)
                                        | Ok(ScriptRecoveryOutcome::ContainmentTerminated)
                                ) && script_execution(
                                    journal,
                                    action_index,
                                    operation_index,
                                )?
                                .containment_empty
                                .is_none()
                                {
                                    script_execution_mut(
                                        journal,
                                        action_index,
                                        operation_index,
                                    )?
                                    .containment_empty = Some(descriptor.confidence);
                                    store.persist(journal)?;
                                }
                                recovered
                            } else {
                                Ok(ScriptRecoveryOutcome::ManualRecoveryRequired(
                                    "durable start record omitted its containment descriptor"
                                        .to_string(),
                                ))
                            };
                            if matches!(
                                &recovery,
                                Ok(ScriptRecoveryOutcome::ExecutionNeverReleased)
                            ) {
                                set_script_terminal(
                                    journal,
                                    action_index,
                                    operation_index,
                                    ScriptTerminalState::SetupFailedBeforeExecution,
                                )?;
                                let summary = format!(
                                    "script supervisor failed after containment preparation: {error}; the validated supervisor result proves the exec gate never opened and no user code ran"
                                );
                                set_script_result(
                                    journal,
                                    action_index,
                                    operation_index,
                                    summary.clone(),
                                    OperationResultStatus::Failed,
                                    None,
                                    None,
                                );
                                transition_operation(
                                    store,
                                    journal,
                                    action_index,
                                    operation_index,
                                    OperationState::FailedDeterministic,
                                )?;
                                return Err(ActionError::Script(summary));
                            }
                            if script_execution(journal, action_index, operation_index)?
                                .output_capture
                                .is_none()
                            {
                                script_execution_mut(
                                    journal,
                                    action_index,
                                    operation_index,
                                )?
                                .output_capture = Some(OutputCaptureSummary {
                                    stdout: OutputCaptureTerminal::Abandoned,
                                    stderr: OutputCaptureTerminal::Abandoned,
                                });
                                store.persist(journal)?;
                            }
                            let (terminal, recovery_detail) = match recovery {
                                Ok(ScriptRecoveryOutcome::ExecutionNeverReleased) => unreachable!(
                                    "exec-gated recovery was handled before ambiguous classification"
                                ),
                                Ok(ScriptRecoveryOutcome::ContainmentAlreadyEmpty)
                                | Ok(ScriptRecoveryOutcome::ContainmentTerminated) => (
                                    ScriptTerminalState::ManualRecoveryRequired,
                                    "the recorded execution domain is now empty".to_string(),
                                ),
                                Ok(ScriptRecoveryOutcome::ManualRecoveryRequired(reason)) => (
                                    ScriptTerminalState::ContainmentUncertain,
                                    reason,
                                ),
                                Err(recovery_error) => (
                                    ScriptTerminalState::ContainmentUncertain,
                                    format!("immediate containment recovery failed: {recovery_error}"),
                                ),
                            };
                            set_script_terminal(
                                journal,
                                action_index,
                                operation_index,
                                terminal,
                            )?;
                            let summary = format!(
                                "script supervisor failed after start was durably recorded: {error}; {recovery_detail}; the script will not be replayed"
                            );
                            set_script_result(
                                journal,
                                action_index,
                                operation_index,
                                summary.clone(),
                                OperationResultStatus::ManualRecoveryRequired,
                                None,
                                None,
                            );
                            transition_operation(
                                store,
                                journal,
                                action_index,
                                operation_index,
                                OperationState::ManualRecoveryRequired,
                            )?;
                            return Err(ActionError::ManualRecoveryRequired(summary));
                        }

                        set_script_terminal(
                            journal,
                            action_index,
                            operation_index,
                            ScriptTerminalState::SetupFailedBeforeExecution,
                        )?;
                        cleanup_unstarted_script_runtime(
                            self.filesystem,
                            &invocation.runtime_directory,
                            invocation.runtime_identity,
                        )?;
                        let summary = format!(
                            "script containment setup failed before user code was released: {error}"
                        );
                        set_script_result(
                            journal,
                            action_index,
                            operation_index,
                            summary.clone(),
                            OperationResultStatus::Failed,
                            None,
                            None,
                        );
                        transition_operation(
                            store,
                            journal,
                            action_index,
                            operation_index,
                            OperationState::FailedDeterministic,
                        )?;
                        return Err(ActionError::Script(summary));
                    }
                };

                if let Err(error) = validate_script_outcome(
                    journal,
                    action_index,
                    operation_index,
                    &invocation,
                    &outcome,
                ) {
                    set_script_terminal(
                        journal,
                        action_index,
                        operation_index,
                        ScriptTerminalState::ContainmentUncertain,
                    )?;
                    let summary = format!(
                        "script supervisor outcome contradicted its durable lifecycle: {error}"
                    );
                    set_script_result(
                        journal,
                        action_index,
                        operation_index,
                        summary.clone(),
                        OperationResultStatus::ManualRecoveryRequired,
                        Some(String::from_utf8_lossy(&outcome.stdout_tail).to_string()),
                        Some(String::from_utf8_lossy(&outcome.stderr_tail).to_string()),
                    );
                    transition_operation(
                        store,
                        journal,
                        action_index,
                        operation_index,
                        OperationState::ManualRecoveryRequired,
                    )?;
                    return Err(ActionError::ManualRecoveryRequired(summary));
                }
                if let Some(warning) = outcome.descriptor.warning.as_ref() {
                    let notice = format!(
                        "runscript containment {} ({}) limitation: {}",
                        outcome.descriptor.backend.as_str(),
                        outcome.descriptor.confidence.as_str(),
                        warning
                    );
                    if !journal.actions[action_index].notices.contains(&notice) {
                        journal.actions[action_index].notices.push(notice);
                        store.persist(journal)?;
                    }
                }
                let stdout_tail = String::from_utf8_lossy(&outcome.stdout_tail).to_string();
                let stderr_tail = String::from_utf8_lossy(&outcome.stderr_tail).to_string();

                if !outcome.started {
                    let terminal = if outcome.cancelled {
                        ScriptTerminalState::SetupFailedBeforeExecution
                    } else {
                        ScriptTerminalState::ContainmentUncertain
                    };
                    set_script_terminal(journal, action_index, operation_index, terminal)?;
                    let summary = if outcome.cancelled {
                        "script was cancelled while exec-gated; no user code was released"
                            .to_string()
                    } else {
                        "script supervisor ended without releasing user code".to_string()
                    };
                    let status = if outcome.cancelled {
                        OperationResultStatus::Skipped
                    } else {
                        OperationResultStatus::Failed
                    };
                    set_script_result(
                        journal,
                        action_index,
                        operation_index,
                        summary.clone(),
                        status,
                        Some(stdout_tail),
                        Some(stderr_tail),
                    );
                    let next = if outcome.cancelled {
                        OperationState::CancelledBeforeMutation
                    } else {
                        OperationState::FailedDeterministic
                    };
                    transition_operation(store, journal, action_index, operation_index, next)?;
                    return if outcome.cancelled {
                        Err(ActionError::CancelledBeforeMutation(summary))
                    } else {
                        Err(ActionError::Script(summary))
                    };
                }

                if !outcome.containment_empty
                    || matches!(
                        outcome.output_capture.stdout,
                        OutputCaptureTerminal::Abandoned
                    )
                    || matches!(
                        outcome.output_capture.stderr,
                        OutputCaptureTerminal::Abandoned
                    )
                {
                    set_script_terminal(
                        journal,
                        action_index,
                        operation_index,
                        ScriptTerminalState::ContainmentUncertain,
                    )?;
                    let summary = format!(
                        "script execution ended without proof that containment and output handles were empty (backend {})",
                        outcome.descriptor.backend.as_str()
                    );
                    set_script_result(
                        journal,
                        action_index,
                        operation_index,
                        summary.clone(),
                        OperationResultStatus::ManualRecoveryRequired,
                        Some(stdout_tail),
                        Some(stderr_tail),
                    );
                    transition_operation(
                        store,
                        journal,
                        action_index,
                        operation_index,
                        OperationState::ManualRecoveryRequired,
                    )?;
                    return Err(ActionError::ManualRecoveryRequired(summary));
                }

                if outcome.background_descendants {
                    set_script_terminal(
                        journal,
                        action_index,
                        operation_index,
                        ScriptTerminalState::BackgroundDescendants,
                    )?;
                    let summary =
                        "script leader exited while descendants remained; descendants were terminated and background execution is rejected"
                            .to_string();
                    set_script_result(
                        journal,
                        action_index,
                        operation_index,
                        summary.clone(),
                        OperationResultStatus::Failed,
                        Some(stdout_tail),
                        Some(stderr_tail),
                    );
                    transition_operation(
                        store,
                        journal,
                        action_index,
                        operation_index,
                        OperationState::FailedDeterministic,
                    )?;
                    return Err(ActionError::Script(summary));
                }

                if outcome.cancelled {
                    set_script_terminal(
                        journal,
                        action_index,
                        operation_index,
                        ScriptTerminalState::CancelledAfterStart,
                    )?;
                    let summary =
                        "script was terminated after cancellation; containment is empty, but user-code side effects may have occurred and the action will not be replayed"
                            .to_string();
                    set_script_result(
                        journal,
                        action_index,
                        operation_index,
                        summary.clone(),
                        OperationResultStatus::ManualRecoveryRequired,
                        Some(stdout_tail),
                        Some(stderr_tail),
                    );
                    transition_operation(
                        store,
                        journal,
                        action_index,
                        operation_index,
                        OperationState::ManualRecoveryRequired,
                    )?;
                    return Err(ActionError::ManualRecoveryRequired(summary));
                }

                if outcome.timed_out || !outcome.status.success() {
                    let terminal = if outcome.timed_out {
                        ScriptTerminalState::TimedOut
                    } else {
                        ScriptTerminalState::ExitFailure
                    };
                    set_script_terminal(journal, action_index, operation_index, terminal)?;
                    let summary = if outcome.timed_out {
                        format!(
                            "script timed out after {} seconds; complete containment was terminated",
                            invocation.timeout.as_secs()
                        )
                    } else {
                        format!("script exited with {}", outcome.status)
                    };
                    set_script_result(
                        journal,
                        action_index,
                        operation_index,
                        summary.clone(),
                        OperationResultStatus::Failed,
                        Some(stdout_tail),
                        Some(stderr_tail),
                    );
                    transition_operation(
                        store,
                        journal,
                        action_index,
                        operation_index,
                        OperationState::FailedDeterministic,
                    )?;
                    return Err(ActionError::Script(summary));
                }

                set_script_terminal(
                    journal,
                    action_index,
                    operation_index,
                    ScriptTerminalState::Success,
                )?;
                set_script_result(
                    journal,
                    action_index,
                    operation_index,
                    format!(
                        "ran {} under {} containment and verified its backend-observable execution domain empty ({})",
                        invocation.script.display(),
                        outcome.descriptor.backend.as_str(),
                        outcome.descriptor.confidence.as_str()
                    ),
                    OperationResultStatus::Completed,
                    Some(stdout_tail),
                    Some(stderr_tail),
                );
                transition_operation(
                    store,
                    journal,
                    action_index,
                    operation_index,
                    OperationState::ScriptCompleted,
                )?;
                self.apply_script(
                    store,
                    journal,
                    action_index,
                    operation_index,
                    invocation,
                    cancellation,
                )
            }
            OperationState::ScriptStartRecorded => {
                let record = script_execution(journal, action_index, operation_index)?.clone();
                let descriptor = match record.descriptor {
                    Some(descriptor) => descriptor,
                    None => {
                        transition_operation(
                            store,
                            journal,
                            action_index,
                            operation_index,
                            OperationState::ManualRecoveryRequired,
                        )?;
                        return Err(ActionError::ManualRecoveryRequired(format!(
                            "script {} has a legacy/partial start record without a stable containment descriptor; automatic replay and signalling are refused",
                            invocation.script.display()
                        )));
                    }
                };
                let request = ScriptRecoveryRequest {
                    token: record.token,
                    runtime_directory: record.runtime_directory,
                    descriptor: descriptor.clone(),
                };
                let mut observer = |event: &ScriptLifecycleEvent| {
                    record_script_recovery_lifecycle_event(
                        store,
                        journal,
                        action_index,
                        operation_index,
                        event,
                    )
                };
                let recovery = self.scripts.recover(&request, &mut observer)?;
                drop(observer);
                if script_execution(journal, action_index, operation_index)?
                    .output_capture
                    .is_none()
                {
                    script_execution_mut(journal, action_index, operation_index)?
                        .output_capture = Some(OutputCaptureSummary {
                        stdout: OutputCaptureTerminal::Abandoned,
                        stderr: OutputCaptureTerminal::Abandoned,
                    });
                    store.persist(journal)?;
                }
                match recovery {
                    ScriptRecoveryOutcome::ExecutionNeverReleased => {
                        {
                            let record = script_execution_mut(
                                journal,
                                action_index,
                                operation_index,
                            )?;
                            if record.containment_empty.is_none() {
                                record.containment_empty = Some(descriptor.confidence);
                            }
                            record.terminal =
                                Some(ScriptTerminalState::SetupFailedBeforeExecution);
                        }
                        let summary = format!(
                            "interrupted script supervisor result proves the exec gate for {} never opened; no user code ran, containment setup failed, and the action will not be replayed",
                            invocation.script.display()
                        );
                        set_script_result(
                            journal,
                            action_index,
                            operation_index,
                            summary.clone(),
                            OperationResultStatus::Failed,
                            None,
                            None,
                        );
                        transition_operation(
                            store,
                            journal,
                            action_index,
                            operation_index,
                            OperationState::FailedDeterministic,
                        )?;
                        Err(ActionError::Script(summary))
                    }
                    ScriptRecoveryOutcome::ContainmentAlreadyEmpty
                    | ScriptRecoveryOutcome::ContainmentTerminated => {
                        {
                            let record = script_execution_mut(
                                journal,
                                action_index,
                                operation_index,
                            )?;
                            if record.containment_empty.is_none() {
                                record.containment_empty = Some(descriptor.confidence);
                            }
                            record.terminal = Some(ScriptTerminalState::ManualRecoveryRequired);
                        }
                        let summary = format!(
                            "interrupted script containment is now empty, but script side effects and exit/output state are ambiguous; {} will not be replayed",
                            invocation.script.display()
                        );
                        set_script_result(
                            journal,
                            action_index,
                            operation_index,
                            summary.clone(),
                            OperationResultStatus::ManualRecoveryRequired,
                            None,
                            None,
                        );
                        transition_operation(
                            store,
                            journal,
                            action_index,
                            operation_index,
                            OperationState::ManualRecoveryRequired,
                        )?;
                        Err(ActionError::ManualRecoveryRequired(summary))
                    }
                    ScriptRecoveryOutcome::ManualRecoveryRequired(reason) => {
                        script_execution_mut(journal, action_index, operation_index)?.terminal =
                            Some(ScriptTerminalState::ContainmentUncertain);
                        set_script_result(
                            journal,
                            action_index,
                            operation_index,
                            reason.clone(),
                            OperationResultStatus::ManualRecoveryRequired,
                            None,
                            None,
                        );
                        transition_operation(
                            store,
                            journal,
                            action_index,
                            operation_index,
                            OperationState::ManualRecoveryRequired,
                        )?;
                        Err(ActionError::ManualRecoveryRequired(reason))
                    }
                }
            }
            OperationState::ScriptCompleted => {
                require_script_terminal_proof(journal, action_index, operation_index)?;
                transition_operation(
                    store,
                    journal,
                    action_index,
                    operation_index,
                    OperationState::Committed,
                )?;
                self.apply_script(
                    store,
                    journal,
                    action_index,
                    operation_index,
                    invocation,
                    cancellation,
                )
            }
            OperationState::Committed => {
                transition_operation(
                    store,
                    journal,
                    action_index,
                    operation_index,
                    OperationState::CleanupStarted,
                )?;
                self.apply_script(
                    store,
                    journal,
                    action_index,
                    operation_index,
                    invocation,
                    cancellation,
                )
            }
            OperationState::CleanupStarted => {
                require_script_terminal_proof(journal, action_index, operation_index)?;
                let record = script_execution(journal, action_index, operation_index)?.clone();
                let descriptor = record.descriptor.ok_or_else(|| {
                    ActionError::InvalidJournal(
                        "script cleanup omitted the containment descriptor".to_string(),
                    )
                })?;
                self.scripts.cleanup(&ScriptRecoveryRequest {
                    token: record.token,
                    runtime_directory: record.runtime_directory.clone(),
                    descriptor,
                })?;
                let runtime_identity = record.runtime_identity.ok_or_else(|| {
                    ActionError::InvalidJournal(
                        "script cleanup omitted the runtime-directory identity".to_string(),
                    )
                })?;
                remove_script_runtime_if_owned(
                    self.filesystem,
                    &record.runtime_directory,
                    runtime_identity,
                )?;
                script_execution_mut(journal, action_index, operation_index)?.cleanup_complete =
                    true;
                transition_operation(
                    store,
                    journal,
                    action_index,
                    operation_index,
                    OperationState::CleanupComplete,
                )?;
                Ok(())
            }
            OperationState::CleanupComplete => Ok(()),
            OperationState::FailedDeterministic => Err(ActionError::Script(
                journal.actions[action_index].operations[operation_index]
                    .result
                    .as_ref()
                    .map(|result| result.summary.clone())
                    .unwrap_or_else(|| "script previously failed".to_string()),
            )),
            OperationState::ManualRecoveryRequired => Err(
                ActionError::ManualRecoveryRequired(
                    journal.actions[action_index].operations[operation_index]
                        .result
                        .as_ref()
                        .map(|result| result.summary.clone())
                        .unwrap_or_else(|| {
                            format!(
                                "script {} is in ambiguous terminal state",
                                invocation.script.display()
                            )
                        }),
                ),
            ),
            OperationState::CancelledBeforeMutation => Err(
                ActionError::CancelledBeforeMutation("script was not started".to_string()),
            ),
            other => Err(ActionError::InvalidJournal(format!(
                "script operation has impossible state {other:?}"
            ))),
        }
    }

    fn apply_rename_transaction(
        &self,
        store: &JournalStore<'_>,
        journal: &mut ActionJournal,
        action_index: usize,
        cancellation: &dyn ActionCancellation,
    ) -> Result<(), ActionError> {
        let transaction = rename_transaction_from_journal(journal, action_index)?;
        for operation_index in transaction.staging_order() {
            if cancellation.is_cancelled() {
                let any_mutation = journal.actions[action_index]
                    .operations
                    .iter()
                    .any(|operation| operation.state.mutation_may_have_started());
                if any_mutation {
                    journal.cancellation = CancellationRecord {
                        requested: true,
                        before_any_mutation: false,
                        recovery_required: true,
                    };
                    store.persist(journal)?;
                    return Err(ActionError::Interrupted(
                        "rename transaction cancelled after staging began".to_string(),
                    ));
                }
                transition_operation(
                    store,
                    journal,
                    action_index,
                    operation_index,
                    OperationState::CancelledBeforeMutation,
                )?;
                return Err(ActionError::CancelledBeforeMutation(
                    "rename transaction was not started".to_string(),
                ));
            }
            self.stage_rename_operation(store, journal, action_index, operation_index)?;
        }

        // No destination is touched until every source is durably staged.
        if !journal.actions[action_index]
            .operations
            .iter()
            .all(|operation| {
                matches!(
                    operation.state,
                    OperationState::RenameStaged
                        | OperationState::RenamePublishStarted
                        | OperationState::RenamePublished
                        | OperationState::Committed
                        | OperationState::CleanupStarted
                        | OperationState::CleanupComplete
                )
            })
        {
            return Err(ActionError::InvalidJournal(
                "rename publication attempted before every source was staged".to_string(),
            ));
        }

        for operation_index in transaction.installation_order() {
            self.publish_rename_operation(store, journal, action_index, operation_index)?;
        }
        for operation_index in 0..journal.actions[action_index].operations.len() {
            loop {
                match journal.actions[action_index].operations[operation_index].state {
                    OperationState::RenamePublished => transition_operation(
                        store,
                        journal,
                        action_index,
                        operation_index,
                        OperationState::Committed,
                    )?,
                    OperationState::Committed => transition_operation(
                        store,
                        journal,
                        action_index,
                        operation_index,
                        OperationState::CleanupStarted,
                    )?,
                    OperationState::CleanupStarted => {
                        // Result rides the CleanupComplete persist (see the
                        // sibling arms): no clean-without-result crash window.
                        set_completed_operation_result(
                            journal,
                            action_index,
                            operation_index,
                        );
                        transition_operation(
                            store,
                            journal,
                            action_index,
                            operation_index,
                            OperationState::CleanupComplete,
                        )?;
                    }
                    OperationState::CleanupComplete => break,
                    other => {
                        return Err(ActionError::InvalidJournal(format!(
                            "rename cleanup has impossible state {other:?}"
                        )));
                    }
                }
            }
        }
        cleanup_empty_rename_staging_roots(self.filesystem, journal, action_index)?;
        Ok(())
    }

    fn stage_rename_operation(
        &self,
        store: &JournalStore<'_>,
        journal: &mut ActionJournal,
        action_index: usize,
        operation_index: usize,
    ) -> Result<(), ActionError> {
        let (source, staging, expected) = match &journal.actions[action_index].operations
            [operation_index]
            .plan
        {
            PlannedOperation::Rename {
                source,
                staging,
                expected_source,
                ..
            } => (source.clone(), staging.clone(), expected_source.clone()),
            _ => {
                return Err(ActionError::InvalidJournal(
                    "non-rename operation in rename transaction".to_string(),
                ));
            }
        };
        let effective_source =
            effective_rename_source(journal, action_index, operation_index, &source)?;
        loop {
            match journal.actions[action_index].operations[operation_index].state {
                OperationState::Prepared => {
                    verify_same_source(self.filesystem, &effective_source, &expected)?;
                    assert_path_absent(self.filesystem, &staging, "rename staging")?;
                    transition_operation(
                        store,
                        journal,
                        action_index,
                        operation_index,
                        OperationState::RenameStageStarted,
                    )?;
                }
                OperationState::RenameStageStarted => {
                    let parent = staging.parent().ok_or_else(|| {
                        ActionError::UnsafePath(format!(
                            "rename staging has no parent: {}",
                            staging.display()
                        ))
                    })?;
                    self.filesystem.create_private_dir_all(parent)?;
                    reconcile_stage_to_witness(
                        self.filesystem,
                        &effective_source,
                        &staging,
                        &expected,
                        "rename source",
                    )?;
                    transition_operation(
                        store,
                        journal,
                        action_index,
                        operation_index,
                        OperationState::RenameStaged,
                    )?;
                }
                OperationState::RenameStaged
                | OperationState::RenamePublishStarted
                | OperationState::RenamePublished
                | OperationState::Committed
                | OperationState::CleanupStarted
                | OperationState::CleanupComplete => return Ok(()),
                other => {
                    return Err(ActionError::InvalidJournal(format!(
                        "rename staging has impossible state {other:?}"
                    )));
                }
            }
        }
    }

    fn publish_rename_operation(
        &self,
        store: &JournalStore<'_>,
        journal: &mut ActionJournal,
        action_index: usize,
        operation_index: usize,
    ) -> Result<(), ActionError> {
        let (destination, staging, expected, source_authority) = match &journal.actions[action_index].operations
            [operation_index]
            .plan
        {
            PlannedOperation::Rename {
                destination,
                staging,
                expected_source,
                expected_staged,
                ..
            } => (
                destination.clone(),
                staging.clone(),
                expected_staged.clone(),
                expected_source.clone(),
            ),
            _ => {
                return Err(ActionError::InvalidJournal(
                    "non-rename operation in rename transaction".to_string(),
                ));
            }
        };
        loop {
            match journal.actions[action_index].operations[operation_index].state {
                OperationState::RenameStaged => {
                    verify_same_content(self.filesystem, &staging, &expected)?;
                    if self.filesystem.path_exists_no_follow(&destination)? {
                        return Err(ActionError::Contradiction(format!(
                            "rename destination appeared after all sources were staged: {}",
                            destination.display()
                        )));
                    }
                    transition_operation(
                        store,
                        journal,
                        action_index,
                        operation_index,
                        OperationState::RenamePublishStarted,
                    )?;
                }
                OperationState::RenamePublishStarted => {
                    let staging_exists = self.filesystem.path_exists_no_follow(&staging)?;
                    let destination_exists =
                        self.filesystem.path_exists_no_follow(&destination)?;
                    match (staging_exists, destination_exists) {
                        (true, false) => {
                            verify_same_content(self.filesystem, &staging, &expected)?;
                            self.filesystem
                                .rename_no_clobber(&staging, &destination, &expected)?;
                        }
                        (false, true) => {
                            let actual =
                                verify_same_content(self.filesystem, &destination, &expected)?;
                            verify_same_filesystem_object_authority(
                                &actual,
                                &source_authority,
                                &destination,
                            )?;
                        }
                        (true, true) => {
                            return Err(ActionError::Contradiction(format!(
                                "rename staging and destination both exist: {} and {}",
                                staging.display(),
                                destination.display()
                            )));
                        }
                        (false, false) => {
                            return Err(ActionError::Contradiction(format!(
                                "rename object is missing from staging and destination: {}",
                                destination.display()
                            )));
                        }
                    }
                    let observed =
                        verify_same_content(self.filesystem, &destination, &expected)?;
                    verify_same_filesystem_object_authority(
                        &observed,
                        &source_authority,
                        &destination,
                    )?;
                    journal.actions[action_index].operations[operation_index]
                        .observed_destination = Some(observed);
                    transition_operation(
                        store,
                        journal,
                        action_index,
                        operation_index,
                        OperationState::RenamePublished,
                    )?;
                }
                OperationState::RenamePublished
                | OperationState::Committed
                | OperationState::CleanupStarted
                | OperationState::CleanupComplete => {
                    verify_observed_destination(
                        self.filesystem,
                        &destination,
                        &expected,
                        journal.actions[action_index].operations[operation_index]
                            .observed_destination
                            .as_ref(),
                    )?;
                    return Ok(());
                }
                other => {
                    return Err(ActionError::InvalidJournal(format!(
                        "rename publication has impossible state {other:?}"
                    )));
                }
            }
        }
    }
}

fn script_execution(
    journal: &ActionJournal,
    action_index: usize,
    operation_index: usize,
) -> Result<&ScriptExecutionJournal, ActionError> {
    journal.actions[action_index].operations[operation_index]
        .script_execution
        .as_ref()
        .ok_or_else(|| {
            ActionError::InvalidJournal(
                "runscript operation omitted its durable execution record".to_string(),
            )
        })
}

fn script_execution_mut(
    journal: &mut ActionJournal,
    action_index: usize,
    operation_index: usize,
) -> Result<&mut ScriptExecutionJournal, ActionError> {
    journal.actions[action_index].operations[operation_index]
        .script_execution
        .as_mut()
        .ok_or_else(|| {
            ActionError::InvalidJournal(
                "runscript operation omitted its durable execution record".to_string(),
            )
        })
}

fn validate_script_execution_plan(
    journal: &ActionJournal,
    action_index: usize,
    operation_index: usize,
    invocation: &ScriptInvocation,
) -> Result<(), ActionError> {
    let record = script_execution(journal, action_index, operation_index)?;
    if record.schema_version != SCRIPT_EXECUTION_SCHEMA_VERSION
        || record.token != invocation.containment_token
        || record.runtime_directory != invocation.runtime_directory
    {
        return Err(ActionError::InvalidJournal(
            "runscript durable execution record does not match its planned invocation"
                .to_string(),
        ));
    }
    if record.start_committed
        || record.user_code_released
        || record.descriptor.is_some()
        || record.termination_requested.is_some()
        || record.forced_termination_requested
        || record.leader_exit_status.is_some()
        || record.containment_empty.is_some()
        || record.output_capture.is_some()
        || record.terminal.is_some()
        || record.cleanup_complete
    {
        return Err(ActionError::InvalidJournal(
            "prepared runscript operation contains impossible execution progress".to_string(),
        ));
    }
    Ok(())
}

fn record_script_recovery_lifecycle_event(
    store: &JournalStore<'_>,
    journal: &mut ActionJournal,
    action_index: usize,
    operation_index: usize,
    event: &ScriptLifecycleEvent,
) -> Result<(), ActionError> {
    match event {
        ScriptLifecycleEvent::ContainmentPrepared { .. }
        | ScriptLifecycleEvent::UserCodeReleased { .. } => {
            return Err(ActionError::InvalidJournal(
                "recovery attempted to prepare or release a new script execution".to_string(),
            ));
        }
        ScriptLifecycleEvent::TerminationRequested {
            reason,
            graceful_deadline_unix_millis,
            ..
        } => {
            if let Some(existing) = script_execution(journal, action_index, operation_index)?
                .termination_requested
                .as_ref()
            {
                // A restart may resume a previously authorized escalation
                // under the synthetic Recovery reason.  Any persisted helper
                // result, however, must reproduce the exact historical reason
                // and deadline rather than silently changing them.
                if *reason == TerminationReason::Recovery
                    || (existing.reason == *reason
                        && existing.graceful_deadline_unix_millis
                            == *graceful_deadline_unix_millis)
                {
                    return Ok(());
                }
                return Err(ActionError::Contradiction(
                    "recovery changed the durable script termination reason or deadline"
                        .to_string(),
                ));
            }
        }
        ScriptLifecycleEvent::ForcedTerminationRequested {
            schema_version,
            reason,
        } => {
            let record = script_execution(journal, action_index, operation_index)?;
            if record.forced_termination_requested {
                if *reason == TerminationReason::Recovery
                    || record
                        .termination_requested
                        .as_ref()
                        .map(|requested| requested.reason == *reason)
                        .unwrap_or(false)
                {
                    return Ok(());
                }
                return Err(ActionError::Contradiction(
                    "recovery changed the durable forced-termination reason".to_string(),
                ));
            }
            if let Some(requested) = record.termination_requested.as_ref() {
                if *reason != TerminationReason::Recovery && requested.reason != *reason {
                    return Err(ActionError::Contradiction(
                        "forced recovery termination reason differs from the durable request"
                            .to_string(),
                    ));
                }
                let normalized = ScriptLifecycleEvent::ForcedTerminationRequested {
                    schema_version: *schema_version,
                    reason: requested.reason,
                };
                return record_script_lifecycle_event(
                    store,
                    journal,
                    action_index,
                    operation_index,
                    &normalized,
                );
            }
        }
        ScriptLifecycleEvent::LeaderExited { raw_wait_status, .. } => {
            if let Some(existing) = script_execution(journal, action_index, operation_index)?
                .leader_exit_status
            {
                if existing == *raw_wait_status {
                    return Ok(());
                }
                return Err(ActionError::Contradiction(
                    "recovery result changed the durable script leader exit status".to_string(),
                ));
            }
        }
        ScriptLifecycleEvent::ContainmentEmpty { confidence, .. } => {
            if let Some(existing) = script_execution(journal, action_index, operation_index)?
                .containment_empty
            {
                if existing == *confidence {
                    return Ok(());
                }
                return Err(ActionError::Contradiction(
                    "recovery changed the durable containment-empty confidence".to_string(),
                ));
            }
        }
        ScriptLifecycleEvent::OutputCaptureCompleted { summary, .. } => {
            if let Some(existing) = script_execution(journal, action_index, operation_index)?
                .output_capture
                .as_ref()
            {
                if existing == summary {
                    return Ok(());
                }
                return Err(ActionError::Contradiction(
                    "recovery changed the durable output-capture terminal state".to_string(),
                ));
            }
        }
    }
    record_script_lifecycle_event(
        store,
        journal,
        action_index,
        operation_index,
        event,
    )
}

fn record_script_lifecycle_event(
    store: &JournalStore<'_>,
    journal: &mut ActionJournal,
    action_index: usize,
    operation_index: usize,
    event: &ScriptLifecycleEvent,
) -> Result<(), ActionError> {
    let current_state = journal.actions[action_index].operations[operation_index].state;
    if !matches!(
        current_state,
        OperationState::Prepared | OperationState::ScriptStartRecorded
    ) {
        return Err(ActionError::InvalidJournal(format!(
            "script lifecycle event arrived in impossible operation state {current_state:?}"
        )));
    }
    let mut prepared_transition = false;
    {
    let record = script_execution_mut(journal, action_index, operation_index)?;
    match event {
        ScriptLifecycleEvent::ContainmentPrepared { descriptor, .. } => {
            if descriptor.token != record.token {
                return Err(ActionError::Contradiction(
                    "script supervisor prepared a containment with a foreign token".to_string(),
                ));
            }
            if record.runtime_identity != Some(descriptor.runtime_directory) {
                return Err(ActionError::Contradiction(
                    "script supervisor prepared containment for a different runtime directory"
                        .to_string(),
                ));
            }
            if let Some(existing) = record.descriptor.as_ref() {
                if existing != descriptor {
                    return Err(ActionError::Contradiction(
                        "script supervisor changed the prepared containment descriptor"
                            .to_string(),
                    ));
                }
            } else {
                record.descriptor = Some(descriptor.clone());
            }
            record.start_committed = true;
            prepared_transition = true;
        }
        ScriptLifecycleEvent::UserCodeReleased { leader, .. } => {
            let descriptor = record.descriptor.as_ref().ok_or_else(|| {
                ActionError::InvalidJournal(
                    "user-code release preceded durable containment preparation".to_string(),
                )
            })?;
            if &descriptor.leader != leader || !record.start_committed {
                return Err(ActionError::Contradiction(
                    "script user-code release did not match the prepared leader identity"
                        .to_string(),
                ));
            }
            record.user_code_released = true;
        }
        ScriptLifecycleEvent::TerminationRequested {
            reason,
            graceful_deadline_unix_millis,
            ..
        } => {
            if !record.start_committed {
                return Err(ActionError::InvalidJournal(
                    "script termination was requested before containment preparation"
                        .to_string(),
                ));
            }
            let next = ScriptTerminationJournal {
                reason: *reason,
                graceful_deadline_unix_millis: *graceful_deadline_unix_millis,
            };
            if let Some(existing) = record.termination_requested.as_ref() {
                if existing != &next {
                    return Err(ActionError::Contradiction(
                        "script supervisor changed the durable termination request"
                            .to_string(),
                    ));
                }
            } else {
                record.termination_requested = Some(next);
            }
        }
        ScriptLifecycleEvent::ForcedTerminationRequested { reason, .. } => {
            let requested = record.termination_requested.as_ref().ok_or_else(|| {
                ActionError::InvalidJournal(
                    "forced script termination preceded the durable graceful request"
                        .to_string(),
                )
            })?;
            if requested.reason != *reason {
                return Err(ActionError::Contradiction(
                    "forced script termination reason differs from the graceful request"
                        .to_string(),
                ));
            }
            record.forced_termination_requested = true;
        }
        ScriptLifecycleEvent::LeaderExited {
            raw_wait_status, ..
        } => {
            if let Some(existing) = record.leader_exit_status {
                if existing != *raw_wait_status {
                    return Err(ActionError::Contradiction(
                        "script supervisor changed the leader exit status".to_string(),
                    ));
                }
            } else {
                record.leader_exit_status = Some(*raw_wait_status);
            }
        }
        ScriptLifecycleEvent::ContainmentEmpty { confidence, .. } => {
            if let Some(existing) = record.containment_empty {
                if existing != *confidence {
                    return Err(ActionError::Contradiction(
                        "script supervisor changed the containment-empty confidence"
                            .to_string(),
                    ));
                }
            } else {
                record.containment_empty = Some(*confidence);
            }
        }
        ScriptLifecycleEvent::OutputCaptureCompleted { summary, .. } => {
            if let Some(existing) = record.output_capture.as_ref() {
                if existing != summary {
                    return Err(ActionError::Contradiction(
                        "script supervisor changed the output-capture terminal state"
                            .to_string(),
                    ));
                }
            } else {
                record.output_capture = Some(summary.clone());
            }
        }
    }
    }
    if prepared_transition {
        journal.actions[action_index].operations[operation_index].state =
            OperationState::ScriptStartRecorded;
    }
    store.persist(journal)
}

fn validate_script_outcome(
    journal: &ActionJournal,
    action_index: usize,
    operation_index: usize,
    invocation: &ScriptInvocation,
    outcome: &ScriptOutcome,
) -> Result<(), ActionError> {
    let record = script_execution(journal, action_index, operation_index)?;
    if record.token != invocation.containment_token
        || record.runtime_directory != invocation.runtime_directory
        || record.descriptor.as_ref() != Some(&outcome.descriptor)
        || !record.start_committed
        || record.user_code_released != outcome.started
        || record.output_capture.as_ref() != Some(&outcome.output_capture)
        || outcome.containment_empty != record.containment_empty.is_some()
    {
        return Err(ActionError::Contradiction(
            "script supervisor outcome does not correspond to the durable lifecycle record"
                .to_string(),
        ));
    }
    if outcome.started && record.leader_exit_status.is_none() {
        return Err(ActionError::Contradiction(
            "script supervisor returned without a durable leader exit status".to_string(),
        ));
    }
    Ok(())
}

fn set_script_terminal(
    journal: &mut ActionJournal,
    action_index: usize,
    operation_index: usize,
    terminal: ScriptTerminalState,
) -> Result<(), ActionError> {
    let record = script_execution_mut(journal, action_index, operation_index)?;
    if let Some(existing) = record.terminal {
        if existing != terminal {
            return Err(ActionError::Contradiction(format!(
                "script terminal state changed from {existing:?} to {terminal:?}"
            )));
        }
    } else {
        record.terminal = Some(terminal);
    }
    Ok(())
}

fn set_script_result(
    journal: &mut ActionJournal,
    action_index: usize,
    operation_index: usize,
    summary: String,
    status: OperationResultStatus,
    stdout_tail: Option<String>,
    stderr_tail: Option<String>,
) {
    let operation_id = journal.actions[action_index].operations[operation_index]
        .operation_id
        .clone();
    journal.actions[action_index].operations[operation_index].result = Some(OperationResult {
        operation_id,
        summary,
        status,
        stdout_tail,
        stderr_tail,
    });
}

fn require_script_terminal_proof(
    journal: &ActionJournal,
    action_index: usize,
    operation_index: usize,
) -> Result<(), ActionError> {
    let record = script_execution(journal, action_index, operation_index)?;
    let output = record.output_capture.as_ref().ok_or_else(|| {
        ActionError::InvalidJournal(
            "successful script operation omitted output-capture completion".to_string(),
        )
    })?;
    if record.terminal != Some(ScriptTerminalState::Success)
        || !record.start_committed
        || !record.user_code_released
        || record.descriptor.is_none()
        || record.leader_exit_status.is_none()
        || record.containment_empty.is_none()
        || matches!(output.stdout, OutputCaptureTerminal::Abandoned)
        || matches!(output.stderr, OutputCaptureTerminal::Abandoned)
    {
        return Err(ActionError::InvalidJournal(
            "script success lacks durable leader-exit, containment-empty, or output-terminal proof"
                .to_string(),
        ));
    }
    Ok(())
}

fn cleanup_unstarted_script_runtime(
    filesystem: &dyn ActionFilesystem,
    runtime_directory: &Path,
    runtime_identity: Option<RuntimeDirectoryIdentity>,
) -> Result<(), ActionError> {
    let Some(runtime_identity) = runtime_identity else {
        return Ok(());
    };
    remove_script_runtime_if_owned(filesystem, runtime_directory, runtime_identity)
}

fn remove_script_runtime_if_owned(
    filesystem: &dyn ActionFilesystem,
    runtime_directory: &Path,
    runtime_identity: RuntimeDirectoryIdentity,
) -> Result<(), ActionError> {
    let expected = CapEntryIdentity {
        file_type: CapFileType::Directory,
        device: runtime_identity.device,
        inode: runtime_identity.inode,
    };
    match filesystem.entry_identity(runtime_directory)? {
        None => {
            // A crash may occur after the descriptor-relative unlink but
            // before the cleanup-complete journal generation. Re-synchronize
            // the retained parent and accept absence; never delete a recreated
            // pathname without the recorded identity.
            filesystem.sync_parent(runtime_directory)?;
        }
        Some(actual) if actual == expected => {
            filesystem.remove_owned_path(runtime_directory, expected)?;
            filesystem.sync_parent(runtime_directory)?;
        }
        Some(actual) => {
            return Err(ActionError::Contradiction(format!(
                "script runtime path {} was replaced before cleanup: expected {:?}, found {:?}",
                runtime_directory.display(),
                expected,
                actual
            )));
        }
    }
    Ok(())
}

fn effective_rename_source(
    journal: &ActionJournal,
    action_index: usize,
    operation_index: usize,
    original_source: &Path,
) -> Result<PathBuf, ActionError> {
    let mut closest: Option<(&Path, &Path)> = None;
    for (candidate_index, operation) in journal.actions[action_index].operations.iter().enumerate() {
        if candidate_index == operation_index {
            continue;
        }
        let PlannedOperation::Rename {
            source: ancestor_source,
            staging: ancestor_staging,
            ..
        } = &operation.plan
        else {
            continue;
        };
        if original_source == ancestor_source || !original_source.starts_with(ancestor_source) {
            continue;
        }
        if !matches!(
            operation.state,
            OperationState::RenameStaged | OperationState::RenamePublishStarted
        ) {
            return Err(ActionError::InvalidJournal(format!(
                "nested rename descendant {} reached staging without a protected ancestor {}",
                original_source.display(),
                ancestor_source.display()
            )));
        }
        let replace = closest
            .as_ref()
            .map(|(current, _)| ancestor_source.components().count() > current.components().count())
            .unwrap_or(true);
        if replace {
            closest = Some((ancestor_source.as_path(), ancestor_staging.as_path()));
        }
    }
    let Some((ancestor_source, ancestor_staging)) = closest else {
        return Ok(original_source.to_path_buf());
    };
    let relative = original_source.strip_prefix(ancestor_source).map_err(|_| {
        ActionError::InvalidJournal("nested rename source rebase failed".to_string())
    })?;
    Ok(ancestor_staging.join(relative))
}

fn rename_transaction_from_journal(
    journal: &ActionJournal,
    action_index: usize,
) -> Result<RenameTransactionPlan, ActionError> {
    let mut entries = Vec::new();
    for operation in &journal.actions[action_index].operations {
        match &operation.plan {
            PlannedOperation::Rename {
                source,
                destination,
                ..
            } => entries.push(crate::convert::rename_plan::RenameTransactionEntry {
                source: source.clone(),
                destination: destination.clone(),
                source_depth: source.components().count(),
                destination_depth: destination.components().count(),
            }),
            _ => {
                return Err(ActionError::InvalidJournal(
                    "rename action contains a non-rename operation".to_string(),
                ));
            }
        }
    }
    let mut destinations = BTreeSet::new();
    for entry in &entries {
        let key = portable_destination_collision_key(&entry.destination);
        if !destinations.insert(key) {
            return Err(ActionError::InvalidJournal(
                "rename journal contains duplicate destination".to_string(),
            ));
        }
    }
    Ok(RenameTransactionPlan {
        entries,
        no_ops: Vec::new(),
    })
}

fn set_completed_operation_result(
    journal: &mut ActionJournal,
    action_index: usize,
    operation_index: usize,
) {
    if journal.actions[action_index].operations[operation_index]
        .result
        .is_some()
    {
        return;
    }
    let operation_id = journal.actions[action_index].operations[operation_index]
        .operation_id
        .clone();
    let summary = operation_summary(
        &journal.actions[action_index].operations[operation_index].plan,
    );
    journal.actions[action_index].operations[operation_index].result = Some(OperationResult {
        operation_id,
        summary,
        status: OperationResultStatus::Completed,
        stdout_tail: None,
        stderr_tail: None,
    });
}

fn verify_same_source(
    filesystem: &dyn ActionFilesystem,
    path: &Path,
    expected: &ObjectIdentity,
) -> Result<ObjectIdentity, ActionError> {
    let actual = identity_matching_policy(filesystem, path, expected)?;
    if !actual.same_object(expected) {
        return Err(ActionError::Contradiction(format!(
            "object identity changed at {} (expected inode/device/content authority)",
            path.display()
        )));
    }
    Ok(actual)
}

fn verify_same_copy_source(
    filesystem: &dyn ActionFilesystem,
    path: &Path,
    expected: &ObjectIdentity,
) -> Result<ObjectIdentity, ActionError> {
    let actual = verify_copy_state_equivalent(filesystem, path, expected)?;
    verify_same_filesystem_object_authority(&actual, expected, path)?;
    Ok(actual)
}

fn verify_relocated_source(
    filesystem: &dyn ActionFilesystem,
    path: &Path,
    expected: &ObjectIdentity,
) -> Result<ObjectIdentity, ActionError> {
    let actual = identity_matching_policy(filesystem, path, expected)?;
    if !actual.copy_state_equivalent(expected) {
        return Err(ActionError::Contradiction(format!(
            "relocated object copy state differs at {}",
            path.display()
        )));
    }
    verify_same_filesystem_object_authority(&actual, expected, path)?;
    Ok(actual)
}

fn verify_same_content(
    filesystem: &dyn ActionFilesystem,
    path: &Path,
    expected: &ObjectIdentity,
) -> Result<ObjectIdentity, ActionError> {
    let actual = identity_matching_policy(filesystem, path, expected)?;
    if !actual.same_content(expected) {
        return Err(ActionError::Contradiction(format!(
            "content identity differs at {}",
            path.display()
        )));
    }
    Ok(actual)
}

fn verify_copy_state_equivalent(
    filesystem: &dyn ActionFilesystem,
    path: &Path,
    expected: &ObjectIdentity,
) -> Result<ObjectIdentity, ActionError> {
    let actual = identity_matching_copy_policy(filesystem, path, expected)?;
    if !actual.copy_state_equivalent(expected) {
        return Err(ActionError::Contradiction(format!(
            "copy state differs at {} (content, mode, or modification time)",
            path.display()
        )));
    }
    Ok(actual)
}

fn verify_same_filesystem_object_authority(
    actual: &ObjectIdentity,
    authority: &ObjectIdentity,
    path: &Path,
) -> Result<(), ActionError> {
    match (
        authority.filesystem.device,
        authority.filesystem.inode,
        actual.filesystem.device,
        actual.filesystem.inode,
    ) {
        (Some(expected_device), Some(expected_inode), Some(device), Some(inode))
            if expected_device == device && expected_inode == inode => Ok(()),
        (Some(_), Some(_), Some(_), Some(_)) => Err(ActionError::Contradiction(format!(
            "filesystem object authority changed at {}",
            path.display()
        ))),
        _ if actual.filesystem == authority.filesystem => Ok(()),
        _ => Err(ActionError::Contradiction(format!(
            "filesystem object authority cannot be proven at {}",
            path.display()
        ))),
    }
}

fn identity_matching_policy(
    filesystem: &dyn ActionFilesystem,
    path: &Path,
    expected: &ObjectIdentity,
) -> Result<ObjectIdentity, ActionError> {
    let without_hidden = filesystem.identity(path, false)?;
    if without_hidden.same_content(expected) {
        return Ok(without_hidden);
    }
    if expected.kind == ObjectKind::Directory {
        let with_hidden = filesystem.identity(path, true)?;
        if with_hidden.same_content(expected) {
            return Ok(with_hidden);
        }
    }
    Ok(without_hidden)
}

fn identity_matching_copy_policy(
    filesystem: &dyn ActionFilesystem,
    path: &Path,
    expected: &ObjectIdentity,
) -> Result<ObjectIdentity, ActionError> {
    let without_hidden = filesystem.identity(path, false)?;
    if without_hidden.copy_state_equivalent(expected) {
        return Ok(without_hidden);
    }
    if expected.kind == ObjectKind::Directory {
        let with_hidden = filesystem.identity(path, true)?;
        if with_hidden.copy_state_equivalent(expected) {
            return Ok(with_hidden);
        }
    }
    Ok(without_hidden)
}

fn identity_includes_hidden(
    filesystem: &dyn ActionFilesystem,
    expected: &ObjectIdentity,
    source: &Path,
) -> Result<bool, ActionError> {
    if expected.kind != ObjectKind::Directory {
        return Ok(false);
    }
    let without_hidden = filesystem.identity(source, false)?;
    Ok(!without_hidden.same_content(expected))
}

fn verify_observed_destination(
    filesystem: &dyn ActionFilesystem,
    destination: &Path,
    expected_content: &ObjectIdentity,
    observed: Option<&ObjectIdentity>,
) -> Result<ObjectIdentity, ActionError> {
    let actual = verify_copy_state_equivalent(filesystem, destination, expected_content)?;
    if let Some(observed) = observed {
        verify_same_filesystem_object_authority(&actual, observed, destination)?;
    }
    Ok(actual)
}

fn verify_content_excluding_publication_witness(
    filesystem: &dyn ActionFilesystem,
    path: &Path,
    witness: &Path,
    expected: &ObjectIdentity,
) -> Result<ObjectIdentity, ActionError> {
    if expected.kind == ObjectKind::File || !filesystem.path_exists_no_follow(witness)? {
        return verify_copy_state_equivalent(filesystem, path, expected);
    }
    let excluded = [witness.to_path_buf()];
    let without_hidden = filesystem.identity_excluding(path, false, &excluded)?;
    if without_hidden.copy_state_equivalent(expected) {
        return Ok(without_hidden);
    }
    let with_hidden = filesystem.identity_excluding(path, true, &excluded)?;
    if with_hidden.copy_state_equivalent(expected) {
        return Ok(with_hidden);
    }
    Err(ActionError::Contradiction(format!(
        "content identity differs at {} after excluding its publication witness",
        path.display()
    )))
}

fn verify_published_destination(
    filesystem: &dyn ActionFilesystem,
    destination: &Path,
    temporary: &Path,
    publication_witness: &Path,
    expected_content: &ObjectIdentity,
    observed: Option<&ObjectIdentity>,
) -> Result<ObjectIdentity, ActionError> {
    if !filesystem.path_exists_no_follow(publication_witness)? {
        return Err(ActionError::Contradiction(format!(
            "protected publication witness is missing: {}",
            publication_witness.display()
        )));
    }
    verify_publication_witness_object(filesystem, publication_witness, destination)?;
    let actual = verify_content_excluding_publication_witness(
        filesystem,
        destination,
        publication_witness,
        expected_content,
    )?;
    if expected_content.kind == ObjectKind::File
        && filesystem.path_exists_no_follow(temporary)?
    {
        let temporary_identity =
            verify_copy_state_equivalent(filesystem, temporary, expected_content)?;
        verify_same_filesystem_object_authority(
            &actual,
            &temporary_identity,
            destination,
        )?;
    }
    if let Some(observed) = observed {
        verify_same_filesystem_object_authority(&actual, observed, destination)?;
    }
    Ok(actual)
}

fn reconcile_stage_to_witness(
    filesystem: &dyn ActionFilesystem,
    original: &Path,
    witness: &Path,
    expected: &ObjectIdentity,
    label: &str,
) -> Result<(), ActionError> {
    let original_exists = filesystem.path_exists_no_follow(original)?;
    let witness_exists = filesystem.path_exists_no_follow(witness)?;
    match (original_exists, witness_exists) {
        (true, false) => {
            verify_same_source(filesystem, original, expected)?;
            filesystem.rename_no_clobber(original, witness, expected)?;
            verify_relocated_source(filesystem, witness, expected)?;
            Ok(())
        }
        (false, true) => {
            verify_relocated_source(filesystem, witness, expected)?;
            Ok(())
        }
        (true, true) => Err(ActionError::Contradiction(format!(
            "{label} and its protected witness both exist: {} and {}",
            original.display(),
            witness.display()
        ))),
        (false, false) => Err(ActionError::Contradiction(format!(
            "{label} is absent from both original and witness paths: {}",
            original.display()
        ))),
    }
}

fn ensure_original_absent_and_witness_matches(
    filesystem: &dyn ActionFilesystem,
    original: &Path,
    witness: &Path,
    expected: &ObjectIdentity,
    label: &str,
) -> Result<(), ActionError> {
    if filesystem.path_exists_no_follow(original)? {
        return Err(ActionError::Contradiction(format!(
            "{label} pathname was recreated after staging: {}",
            original.display()
        )));
    }
    verify_relocated_source(filesystem, witness, expected)?;
    Ok(())
}

fn assert_path_absent(
    filesystem: &dyn ActionFilesystem,
    path: &Path,
    label: &str,
) -> Result<(), ActionError> {
    if filesystem.path_exists_no_follow(path)? {
        return Err(ActionError::Contradiction(format!(
            "{label} unexpectedly exists: {}",
            path.display()
        )));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PublicationWitness {
    schema_version: u32,
    claim_id: String,
    operation_id: String,
    expected_content_sha256: String,
    expected_kind: ObjectKind,
    publication_filesystem: FilesystemIdentity,
}

fn write_publication_witness(
    filesystem: &dyn ActionFilesystem,
    path: &Path,
    publication_source: &Path,
    claim_id: &str,
    operation_id: &str,
    expected: &ObjectIdentity,
) -> Result<(), ActionError> {
    let witness = PublicationWitness {
        schema_version: 1,
        claim_id: claim_id.to_string(),
        operation_id: operation_id.to_string(),
        expected_content_sha256: expected.content_sha256.clone(),
        expected_kind: expected.kind,
        publication_filesystem: filesystem.identity(publication_source, true)?.filesystem,
    };
    let bytes = serde_json::to_vec_pretty(&witness).map_err(ActionError::Serialization)?;
    filesystem.write_bytes_create_new_durable(path, &bytes)
}

fn verify_publication_witness_object(
    filesystem: &dyn ActionFilesystem,
    witness_path: &Path,
    published_path: &Path,
) -> Result<(), ActionError> {
    let witness: PublicationWitness = serde_json::from_slice(&filesystem.read_bytes(witness_path)?)
        .map_err(|error| ActionError::InvalidJournal(error.to_string()))?;
    let actual = filesystem.identity(published_path, true)?.filesystem;
    match (
        witness.publication_filesystem.device,
        witness.publication_filesystem.inode,
        actual.device,
        actual.inode,
    ) {
        (Some(expected_device), Some(expected_inode), Some(device), Some(inode))
            if expected_device == device && expected_inode == inode => Ok(()),
        (Some(_), Some(_), Some(_), Some(_)) => Err(ActionError::Contradiction(format!(
            "published object does not match the protected publication identity: {}",
            published_path.display()
        ))),
        _ => Ok(()),
    }
}

fn validate_publication_witness(
    filesystem: &dyn ActionFilesystem,
    path: &Path,
    claim_id: &str,
    operation_id: &str,
    expected: &ObjectIdentity,
) -> Result<CapEntryIdentity, ActionError> {
    let (bytes, entry_identity) = filesystem
        .read_bytes_with_identity_optional(path)?
        .ok_or_else(|| ActionError::Contradiction(format!(
            "publication witness is missing at {}",
            path.display()
        )))?;
    let witness: PublicationWitness = serde_json::from_slice(&bytes)
        .map_err(|error| ActionError::InvalidJournal(error.to_string()))?;
    if witness.schema_version != 1
        || witness.claim_id != claim_id
        || witness.operation_id != operation_id
        || witness.expected_content_sha256 != expected.content_sha256
        || witness.expected_kind != expected.kind
    {
        return Err(ActionError::Contradiction(format!(
            "publication witness authority mismatch: {}",
            path.display()
        )));
    }
    Ok(entry_identity)
}

fn cleanup_empty_rename_staging_roots(
    filesystem: &dyn ActionFilesystem,
    journal: &ActionJournal,
    action_index: usize,
) -> Result<(), ActionError> {
    let mut roots = BTreeSet::new();
    for operation in &journal.actions[action_index].operations {
        if let PlannedOperation::Rename { staging, .. } = &operation.plan {
            if let Some(parent) = staging.parent() {
                roots.insert(parent.to_path_buf());
            }
        }
    }
    for root in roots.into_iter().rev() {
        if !filesystem.path_exists_no_follow(&root)? {
            continue;
        }
        let root_identity = filesystem.entry_identity(&root)?.ok_or_else(|| {
            ActionError::Contradiction(format!(
                "rename staging root vanished during cleanup: {}",
                root.display()
            ))
        })?;
        if filesystem.directory_is_empty(&root)? {
            filesystem.remove_owned_path(&root, root_identity)?;
        }
    }
    Ok(())
}

fn cleanup_terminal_script_artifacts(
    scripts: &dyn ActionScriptRunner,
    filesystem: &dyn ActionFilesystem,
    store: &JournalStore<'_>,
    journal: &mut ActionJournal,
) -> Result<(), ActionError> {
    for action_index in 0..journal.actions.len() {
        for operation_index in 0..journal.actions[action_index].operations.len() {
            let state = journal.actions[action_index].operations[operation_index].state;
            if !matches!(
                state,
                OperationState::FailedDeterministic
                    | OperationState::CancelledBeforeMutation
                    | OperationState::ManualRecoveryRequired
            ) {
                continue;
            }
            let Some(record) = journal.actions[action_index].operations[operation_index]
                .script_execution
                .clone()
            else {
                continue;
            };
            if record.cleanup_complete {
                continue;
            }
            let safe_without_descriptor = !record.start_committed
                && !record.user_code_released
                && matches!(
                    record.terminal,
                    Some(ScriptTerminalState::SetupFailedBeforeExecution)
                );
            let safe_with_descriptor = record.descriptor.is_some()
                && record.containment_empty.is_some()
                && record.terminal.is_some();
            if !safe_without_descriptor && !safe_with_descriptor {
                return Err(ActionError::ManualRecoveryRequired(format!(
                    "cannot clean script runtime {} without durable containment-empty proof",
                    record.runtime_directory.display()
                )));
            }
            if let Some(descriptor) = record.descriptor.clone() {
                scripts.cleanup(&ScriptRecoveryRequest {
                    token: record.token.clone(),
                    runtime_directory: record.runtime_directory.clone(),
                    descriptor,
                })?;
            }
            let runtime_identity = record.runtime_identity.ok_or_else(|| {
                ActionError::InvalidJournal(
                    "terminal script cleanup omitted the runtime-directory identity".to_string(),
                )
            })?;
            remove_script_runtime_if_owned(
                filesystem,
                &record.runtime_directory,
                runtime_identity,
            )?;
            script_execution_mut(journal, action_index, operation_index)?.cleanup_complete = true;
            store.persist(journal)?;
        }
    }
    Ok(())
}

fn cleanup_phase_artifacts(
    filesystem: &dyn ActionFilesystem,
    store: &JournalStore<'_>,
    journal: &mut ActionJournal,
) -> Result<bool, ActionError> {
    if journal.actions.iter().flat_map(|action| &action.operations).any(|operation| {
        operation
            .script_execution
            .as_ref()
            .map(|record| operation.state.terminal() && !record.cleanup_complete)
            .unwrap_or(false)
    }) {
        return Ok(false);
    }
    for action_index in 0..journal.actions.len() {
        if journal.actions[action_index].state == JournalActionState::Completed {
            for operation_index in 0..journal.actions[action_index].operations.len() {
                if journal.actions[action_index].operations[operation_index].state
                    != OperationState::CleanupComplete
                {
                    return Ok(false);
                }
            }
        }
    }
    // Remove only validated, journal-owned paths that are now empty or absent.
    // Never use remove_dir_all on a broad coordination workspace.
    let mut directories: Vec<PathBuf> = journal.workspace_paths.clone();
    directories.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    directories.dedup();
    for directory in directories {
        if !is_action_owned_path(&directory, &journal.claim_id)
            || !filesystem.path_exists_no_follow(&directory)?
        {
            continue;
        }
        let identity = filesystem.identity(&directory, true)?;
        if identity.kind == ObjectKind::Directory && filesystem.directory_is_empty(&directory)? {
            filesystem.remove_owned_path(&directory, cap_entry_identity(&identity)?)?;
        }
    }
    // Persist terminal cleanup through the caller. The journal itself remains
    // as the durable rerun/election result and is intentionally not swept by a
    // generic stale-workspace cleaner.
    store.persist(journal)?;
    Ok(true)
}

fn finalize_terminal(
    store: &JournalStore<'_>,
    journal: &mut ActionJournal,
    report: ActionPhaseReport,
    cleanup_complete: bool,
) -> Result<(), ActionError> {
    journal.terminal = Some(TerminalJournalResult {
        report,
        cleanup_complete,
    });
    store.persist(journal)
}

fn report_from_journal(journal: &ActionJournal) -> Result<ActionPhaseReport, ActionError> {
    let mut report = ActionPhaseReport {
        phase: Some(journal.phase),
        actions: Vec::new(),
        notices: Vec::new(),
        recovery_required: journal.cancellation.recovery_required,
        cancelled: journal.cancellation.requested,
    };
    for action in &journal.actions {
        let status = match action.state {
            JournalActionState::Completed => ActionResultStatus::Completed,
            JournalActionState::NoOp => ActionResultStatus::NoOp,
            JournalActionState::FailedDeterministic => ActionResultStatus::Failed,
            JournalActionState::SkippedAfterFailure => ActionResultStatus::SkippedAfterFailure,
            JournalActionState::CancelledBeforeMutation => {
                ActionResultStatus::CancelledBeforeMutation
            }
            JournalActionState::InterruptedRecoverable => {
                report.recovery_required = true;
                ActionResultStatus::Interrupted
            }
            JournalActionState::ManualRecoveryRequired => {
                report.recovery_required = true;
                ActionResultStatus::ManualRecoveryRequired
            }
            JournalActionState::Pending
            | JournalActionState::Planned
            | JournalActionState::Running => {
                report.recovery_required = true;
                ActionResultStatus::Interrupted
            }
        };
        let parsed: ConversionAction = serde_json::from_str(&action.action_serialized)
            .map_err(ActionError::Serialization)?;
        report.actions.push(ActionResult {
            index: action.index,
            kind: parsed.kind_name().to_string(),
            status,
            operations: action
                .operations
                .iter()
                .filter_map(|operation| operation.result.clone())
                .collect(),
            error: action.error.clone(),
            notices: action.notices.clone(),
        });
        report.notices.extend(action.notices.clone());
    }
    Ok(report)
}

fn validate_terminal_report(
    journal: &ActionJournal,
    terminal: &TerminalJournalResult,
) -> Result<(), ActionError> {
    let expected = report_from_journal(journal)?;
    if terminal.report != expected {
        return Err(ActionError::InvalidJournal(
            "terminal report does not correspond to journal action results".to_string(),
        ));
    }
    if terminal.cleanup_complete
        && journal.actions.iter().any(|action| {
            action.state == JournalActionState::Completed
                && action.operations.iter().any(|operation| {
                    operation.state != OperationState::CleanupComplete
                })
        })
    {
        return Err(ActionError::InvalidJournal(
            "terminal claims cleanup complete while operations remain unclean".to_string(),
        ));
    }
    Ok(())
}

fn validate_terminal_journal_authority_without_write_temporary(
    journal: &ActionJournal,
    journal_path: &Path,
) -> Result<(), ActionError> {
    if journal.schema_version != JOURNAL_SCHEMA_VERSION
        || journal.generation == 0
        || Uuid::parse_str(&journal.claim_id).is_err()
        || journal.journal_path != journal_path
        || journal.pipeline_sha256 != sha256_hex(journal.pipeline_serialized.as_bytes())
    {
        return Err(ActionError::InvalidJournal(
            "terminal journal identity is malformed or self-inconsistent".to_string(),
        ));
    }
    let expected_temporary = journal_write_temporary_path(journal_path)?;
    if journal.journal_write_temporary != expected_temporary {
        return Err(ActionError::InvalidJournal(
            "terminal journal write-temporary path is foreign".to_string(),
        ));
    }
    if journal.actions.iter().enumerate().any(|(index, action)| {
        action.index != index
            || action.action_sha256 != sha256_hex(action.action_serialized.as_bytes())
            || !action.state.terminal()
    }) {
        return Err(ActionError::InvalidJournal(
            "terminal journal contains a malformed or non-terminal action slot".to_string(),
        ));
    }
    for (action_index, action) in journal.actions.iter().enumerate() {
        validate_action_state_consistency(action)?;
        if action.plan.is_none() && !action.operations.is_empty() {
            return Err(ActionError::InvalidJournal(format!(
                "terminal action {} has operations without a plan",
                action_index + 1
            )));
        }
        if let Some(plan) = action.plan.as_ref() {
            validate_planning_precondition_identities(plan)?;
            if plan.operations.len() != action.operations.len() {
                return Err(ActionError::InvalidJournal(format!(
                    "terminal action {} plan/operation cardinality mismatch",
                    action_index + 1
                )));
            }
            for (operation_index, (planned, operation)) in
                plan.operations.iter().zip(&action.operations).enumerate()
            {
                if planned != &operation.plan
                    || operation.kind != operation.plan.kind()
                    || operation.operation_id
                        != operation_id(&journal.claim_id, action_index, operation_index)
                {
                    return Err(ActionError::InvalidJournal(format!(
                        "terminal action {} operation {} identity mismatch",
                        action_index + 1,
                        operation_index + 1
                    )));
                }
                validate_operation_state(operation)?;
            }
        }
    }
    let authorized_workspaces = authorized_workspace_paths(journal);
    let recorded_workspaces = journal.workspace_paths.iter().cloned().collect::<BTreeSet<_>>();
    if authorized_workspaces != recorded_workspaces
        || journal.workspace_paths.len() != recorded_workspaces.len()
        || journal.workspace_paths.len() != journal.workspace_capability_paths.len()
    {
        return Err(ActionError::InvalidJournal(
            "terminal journal workspace inventory is inconsistent".to_string(),
        ));
    }
    for path in &journal.workspace_paths {
        validate_workspace_path(path, &journal.claim_id)?;
        match fs::symlink_metadata(path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Ok(_) => {
                return Err(ActionError::InvalidJournal(format!(
                    "terminal journal still owns a recovery artifact: {}",
                    path.display()
                )));
            }
            Err(error) => {
                return Err(ActionError::InvalidJournal(format!(
                    "terminal journal recovery artifact is unreadable at {}: {error}",
                    path.display()
                )));
            }
        }
    }
    let terminal = journal.terminal.as_ref().ok_or_else(|| {
        ActionError::InvalidJournal("journal has no terminal report".to_string())
    })?;
    if !terminal.cleanup_complete || terminal.report.recovery_required {
        return Err(ActionError::InvalidJournal(
            "terminal journal still requires cleanup or recovery".to_string(),
        ));
    }
    validate_terminal_report(journal, terminal)
}

fn validate_resolved_terminal_journal_authority(
    journal: &ActionJournal,
    journal_path: &Path,
) -> Result<(), ActionError> {
    validate_terminal_journal_authority_without_write_temporary(journal, journal_path)?;
    let expected_temporary = journal_write_temporary_path(journal_path)?;
    match fs::symlink_metadata(&expected_temporary) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Ok(_) => Err(ActionError::InvalidJournal(
            "terminal journal still has an unresolved write-temporary generation".to_string(),
        )),
        Err(error) => Err(ActionError::InvalidJournal(format!(
            "terminal journal write-temporary state is unreadable: {error}"
        ))),
    }
}

fn validate_resolved_terminal_journal_authority_for_context(
    filesystem: &dyn ActionFilesystem,
    journal: &ActionJournal,
    journal_path: &Path,
    retained_live_context: bool,
) -> Result<(), ActionError> {
    if !retained_live_context {
        return validate_resolved_terminal_journal_authority(journal, journal_path);
    }
    validate_terminal_journal_authority_without_write_temporary(journal, journal_path)?;
    let expected_temporary = journal_write_temporary_path(journal_path)?;
    if filesystem.path_exists_no_follow(&expected_temporary)? {
        return Err(ActionError::InvalidJournal(
            "terminal journal still has an unresolved write-temporary generation".to_string(),
        ));
    }
    Ok(())
}

fn validate_journal(
    journal: &ActionJournal,
    filesystem: &dyn ActionFilesystem,
    context: &ActionContext,
    pipeline: &ActionPipeline,
    pipeline_serialized: &str,
    pipeline_sha256: &str,
) -> Result<(), ActionError> {
    if journal.schema_version != JOURNAL_SCHEMA_VERSION {
        return Err(ActionError::InvalidJournal(format!(
            "unsupported schema version {}",
            journal.schema_version
        )));
    }
    if journal.generation == 0 {
        return Err(ActionError::InvalidJournal(
            "journal generation must be positive".to_string(),
        ));
    }
    if Uuid::parse_str(&journal.claim_id).is_err() {
        return Err(ActionError::InvalidJournal(
            "journal claim identity is not a valid UUID".to_string(),
        ));
    }
    for (label, path) in [
        ("journal subject", journal.subject_dir.as_path()),
        ("journal source", journal.source_path.as_path()),
        ("journal output", journal.output_root.as_path()),
        ("journal album", journal.album_dir.as_path()),
        ("journal storage", journal.journal_path.as_path()),
        (
            "journal temporary storage",
            journal.journal_write_temporary.as_path(),
        ),
    ] {
        reject_ephemeral_descriptor_namespace_path(label, path)?;
    }
    for record in &journal.capability_roots {
        reject_ephemeral_descriptor_namespace_path(
            "journal capability-root logical path",
            &record.logical_path,
        )?;
    }
    filesystem
        .validate_scope_records(&journal.capability_roots)
        .map_err(|error| ActionError::InvalidJournal(format!(
            "journal capability roots do not match retained descriptors: {error}"
        )))?;
    // Album-batch action journals are album-scoped: one elected participant
    // writes the journal, and EVERY participant of the same run/album may
    // validate it during finalize. Those readers legitimately carry their own
    // track as `source_path`; require it to be a member of the same validated
    // subject directory instead of the elected track. Explicit/manual scopes
    // keep the exact single-context binding.
    let source_path_matches = journal.source_path == context.source_path
        || (!context.explicit_scope
            && match context.batch_source_scope_root.as_deref() {
                // Album batch: any member track under the shared grouping
                // root may validate the elected writer's journal.
                Some(root) => {
                    journal.source_path.starts_with(root)
                        && context.source_path.starts_with(root)
                }
                None => {
                    journal.source_path.parent().is_some()
                        && journal.source_path.parent() == context.source_path.parent()
                }
            });
    if journal.run_identity != context.run_identity
        || journal.album_identity != context.album_identity
        || journal.phase != context.phase
        || journal.subject_dir != context.subject_dir
        || !source_path_matches
        || journal.output_root != context.output_root
        || journal.album_dir != context.album_dir
    {
        return Err(ActionError::InvalidJournal(
            "run/album/phase/context identity mismatch".to_string(),
        ));
    }
    if journal.pipeline_serialized != pipeline_serialized
        || journal.pipeline_sha256 != pipeline_sha256
        || sha256_hex(journal.pipeline_serialized.as_bytes()) != journal.pipeline_sha256
    {
        return Err(ActionError::InvalidJournal(
            "configured pipeline differs from durable journal identity".to_string(),
        ));
    }
    let actions = pipeline.for_phase(context.phase);
    if journal.actions.len() != actions.len() {
        return Err(ActionError::InvalidJournal(
            "action slot count differs from configured pipeline".to_string(),
        ));
    }
    let expected_journal_path = action_journal_path(context, pipeline_sha256)?;
    let expected_temporary = journal_write_temporary_path(&expected_journal_path)?;
    if journal.journal_path != expected_journal_path
        || journal.journal_write_temporary != expected_temporary
    {
        return Err(ActionError::InvalidJournal(
            "journal-provided storage path is foreign".to_string(),
        ));
    }
    if journal.journal_scoped_path != filesystem.scoped_path(&expected_journal_path)?
        || journal.journal_write_temporary_scoped_path
            != filesystem.scoped_path(&expected_temporary)?
    {
        return Err(ActionError::InvalidJournal(
            "journal capability storage authority is foreign".to_string(),
        ));
    }
    if journal.workspace_paths.len() != journal.workspace_capability_paths.len() {
        return Err(ActionError::InvalidJournal(
            "journal workspace path/capability cardinality mismatch".to_string(),
        ));
    }
    for (display, authority) in journal
        .workspace_paths
        .iter()
        .zip(&journal.workspace_capability_paths)
    {
        if authority != &filesystem.scoped_path(display)? {
            return Err(ActionError::InvalidJournal(format!(
                "journal workspace capability mismatch: {}",
                display.display()
            )));
        }
    }
    validate_mutation_path(&journal.journal_path, false)?;
    validate_mutation_path(&journal.journal_write_temporary, false)?;

    // Per-operation renderings (script environment, template expansion) were
    // produced by the ELECTED writer's context. A batch reader validating with
    // its own member track must compare against the writer's view, so
    // substitute the journal's already-validated source before the loop.
    let elected_view;
    let operation_context = if journal.source_path == context.source_path {
        context
    } else {
        let mut view = context.clone();
        view.source_path = journal.source_path.clone();
        // Batch members are per-track FILE jobs; a directory source occurs
        // only when the subject itself was the source (folder-source shape),
        // which the equality below reconstructs from the journal's own
        // already-validated fields.
        view.source_is_directory = journal.source_path == journal.subject_dir;
        elected_view = view;
        &elected_view
    };

    for (index, (slot, configured)) in journal.actions.iter().zip(actions).enumerate() {
        if slot.index != index {
            return Err(ActionError::InvalidJournal(
                "action slots are not in deterministic order".to_string(),
            ));
        }
        let serialized = serde_json::to_string(configured)
            .map_err(ActionError::Serialization)?;
        if slot.action_serialized != serialized
            || slot.action_sha256 != sha256_hex(serialized.as_bytes())
            || slot.continue_on_error != configured.continue_on_error()
        {
            return Err(ActionError::InvalidJournal(format!(
                "action identity mismatch at slot {}",
                index + 1
            )));
        }
        if slot.plan.is_none() && !slot.operations.is_empty() {
            return Err(ActionError::InvalidJournal(format!(
                "action {} has operations without a plan",
                index + 1
            )));
        }
        if let Some(plan) = &slot.plan {
            if plan.action_kind != configured.kind_name()
                || plan.operations.len() != slot.operations.len()
            {
                return Err(ActionError::InvalidJournal(format!(
                    "action {} plan/operation correspondence mismatch",
                    index + 1
                )));
            }
            validate_planning_precondition_shapes(configured, plan, operation_context)?;
            validate_planning_precondition_identities(plan)?;
            for (operation_index, (planned, durable)) in plan
                .operations
                .iter()
                .zip(&slot.operations)
                .enumerate()
            {
                if planned != &durable.plan
                    || durable.kind != durable.plan.kind()
                    || durable.operation_id
                        != operation_id(&journal.claim_id, index, operation_index)
                {
                    return Err(ActionError::InvalidJournal(format!(
                        "action {} operation {} identity mismatch",
                        index + 1,
                        operation_index + 1
                    )));
                }
                validate_operation_paths(
                    durable,
                    configured,
                    operation_context,
                    &journal.claim_id,
                    index,
                    operation_index,
                    journal.source_path != context.source_path,
                )?;
                let expected_capability_paths =
                    capability_paths_for_operation(filesystem, &durable.plan)?;
                if durable.capability_paths != expected_capability_paths {
                    return Err(ActionError::InvalidJournal(format!(
                        "action {} operation {} capability authority mismatch",
                        index + 1,
                        operation_index + 1
                    )));
                }
                validate_operation_state(durable)?;
            }
        }
        validate_action_state_consistency(slot)?;
    }

    if let Some(stop) = &journal.stop_decision {
        if stop.failed_action_index >= journal.actions.len() {
            return Err(ActionError::InvalidJournal(
                "stop decision names an invalid action".to_string(),
            ));
        }
        if !matches!(
            journal.actions[stop.failed_action_index].state,
            JournalActionState::FailedDeterministic
                | JournalActionState::ManualRecoveryRequired
                | JournalActionState::InterruptedRecoverable
        ) {
            return Err(ActionError::InvalidJournal(
                "stop decision is not backed by a failed/interrupted action".to_string(),
            ));
        }
        if stop.remainder_marked_skipped
            && journal
                .actions
                .iter()
                .skip(stop.failed_action_index + 1)
                .any(|action| action.state != JournalActionState::SkippedAfterFailure)
        {
            return Err(ActionError::InvalidJournal(
                "stop decision claims skipped remainder but later action is executable"
                    .to_string(),
            ));
        }
    }
    let authorized_workspaces = authorized_workspace_paths(journal);
    let journal_workspaces: BTreeSet<_> = journal.workspace_paths.iter().cloned().collect();
    if journal_workspaces != authorized_workspaces
        || journal.workspace_paths.len() != journal_workspaces.len()
    {
        return Err(ActionError::InvalidJournal(
            "journal workspace inventory differs from operation-owned recovery paths".to_string(),
        ));
    }
    for path in &journal.workspace_paths {
        validate_workspace_path(path, &journal.claim_id)?;
    }
    if let Some(terminal) = &journal.terminal {
        validate_terminal_report(journal, terminal)?;
    }
    Ok(())
}

fn validate_action_state_consistency(action: &JournalAction) -> Result<(), ActionError> {
    let has_operations = !action.operations.is_empty();
    let requires_roots = operation_roots_require_materialization(&action.operations);
    match action.root_materialization {
        RootMaterializationState::NotStarted => {
            if matches!(action.state, JournalActionState::Running | JournalActionState::Completed) {
                return Err(ActionError::InvalidJournal(
                    "running/completed action has no destination-root mutation state".to_string(),
                ));
            }
        }
        RootMaterializationState::NotRequired => {
            if !has_operations || requires_roots {
                return Err(ActionError::InvalidJournal(
                    "action marks destination roots not required despite non-script or empty operations".to_string(),
                ));
            }
        }
        RootMaterializationState::Started => {
            if !requires_roots
                || !matches!(
                    action.state,
                    JournalActionState::Running | JournalActionState::InterruptedRecoverable
                )
            {
                return Err(ActionError::InvalidJournal(
                    "destination-root materialization start is inconsistent with action operations/state".to_string(),
                ));
            }
        }
        RootMaterializationState::Complete => {
            if !requires_roots {
                return Err(ActionError::InvalidJournal(
                    "action marks destination-root materialization complete without a filesystem operation".to_string(),
                ));
            }
        }
    }

    match action.state {
        JournalActionState::Pending if action.plan.is_some() => Err(ActionError::InvalidJournal(
            "pending action already has a plan".to_string(),
        )),
        JournalActionState::Planned | JournalActionState::Running
            if action.plan.is_none() =>
        {
            Err(ActionError::InvalidJournal(
                "planned/running action has no concrete plan".to_string(),
            ))
        }
        JournalActionState::Completed
            if action
                .operations
                .iter()
                .any(|operation| operation.state != OperationState::CleanupComplete) =>
        {
            Err(ActionError::InvalidJournal(
                "completed action contains a non-clean operation".to_string(),
            ))
        }
        JournalActionState::NoOp if !action.operations.is_empty() => Err(
            ActionError::InvalidJournal("no-op action contains operations".to_string()),
        ),
        JournalActionState::FailedDeterministic if action.error.is_none() => Err(
            ActionError::InvalidJournal("failed action has no error".to_string()),
        ),
        _ => Ok(()),
    }
}

fn validate_operation_state(operation: &JournalOperation) -> Result<(), ActionError> {
    if operation.kind != operation.plan.kind() {
        return Err(ActionError::InvalidJournal(
            "operation kind does not match serialized plan".to_string(),
        ));
    }
    validate_plan_identities(&operation.plan)?;
    match operation.kind {
        OperationKind::RunScript => validate_script_operation_state(operation)?,
        _ if operation.script_execution.is_some() => {
            return Err(ActionError::InvalidJournal(
                "non-script operation contains script execution state".to_string(),
            ));
        }
        _ => {}
    }
    if let Some(identity) = &operation.observed_destination {
        validate_object_identity(identity)?;
    }
    if let PlannedOperation::RepairCopyMetadata {
        expected_source,
        expected_destination,
        ..
    } = &operation.plan
    {
        let repair_has_completed = matches!(
            operation.state,
            OperationState::MetadataRepaired
                | OperationState::Committed
                | OperationState::CleanupStarted
                | OperationState::CleanupComplete
        );
        if repair_has_completed {
            let observed = operation.observed_destination.as_ref().ok_or_else(|| {
                ActionError::InvalidJournal(
                    "completed copy metadata repair omitted observed destination identity"
                        .to_string(),
                )
            })?;
            if !observed.copy_state_equivalent(expected_source) {
                return Err(ActionError::InvalidJournal(
                    "copy metadata repair observed destination is not canonical".to_string(),
                ));
            }
            verify_same_filesystem_object_authority(
                observed,
                expected_destination,
                Path::new("<journal-copy-metadata-destination>"),
            )
            .map_err(|error| ActionError::InvalidJournal(error.to_string()))?;
        }
    }
    if operation.state == OperationState::CleanupComplete && operation.result.is_none() {
        return Err(ActionError::InvalidJournal(format!(
            "clean operation {} has no result",
            operation.operation_id
        )));
    }
    if let Some(result) = &operation.result {
        if result.operation_id != operation.operation_id {
            return Err(ActionError::InvalidJournal(
                "operation result belongs to a different operation".to_string(),
            ));
        }
        let expected = match operation.state {
            OperationState::CleanupComplete => Some(OperationResultStatus::Completed),
            OperationState::FailedDeterministic => Some(OperationResultStatus::Failed),
            OperationState::ManualRecoveryRequired => {
                Some(OperationResultStatus::ManualRecoveryRequired)
            }
            OperationState::CancelledBeforeMutation => Some(OperationResultStatus::Skipped),
            OperationState::InterruptedRecoverable => Some(OperationResultStatus::Interrupted),
            _ => None,
        };
        if let Some(expected) = expected {
            if result.status != expected {
                return Err(ActionError::InvalidJournal(format!(
                    "operation {} state/result status mismatch",
                    operation.operation_id
                )));
            }
        }
    }
    Ok(())
}

fn validate_script_operation_state(operation: &JournalOperation) -> Result<(), ActionError> {
    let PlannedOperation::RunScript {
        runtime_directory,
        containment_token,
        ..
    } = &operation.plan
    else {
        return Err(ActionError::InvalidJournal(
            "script operation kind has a non-script plan".to_string(),
        ));
    };
    let record = operation.script_execution.as_ref().ok_or_else(|| {
        ActionError::InvalidJournal(
            "runscript operation omitted its execution journal".to_string(),
        )
    })?;
    if record.schema_version != SCRIPT_EXECUTION_SCHEMA_VERSION
        || &record.token != containment_token
        || &record.runtime_directory != runtime_directory
        || (operation.state == OperationState::CleanupComplete && !record.cleanup_complete)
        || (record.cleanup_complete
            && !matches!(
                operation.state,
                OperationState::CleanupComplete
                    | OperationState::FailedDeterministic
                    | OperationState::CancelledBeforeMutation
                    | OperationState::ManualRecoveryRequired
            ))
    {
        return Err(ActionError::InvalidJournal(
            "runscript execution journal identity/cleanup state mismatch".to_string(),
        ));
    }
    if record.user_code_released && (!record.start_committed || record.descriptor.is_none()) {
        return Err(ActionError::InvalidJournal(
            "runscript journal claims user-code release without durable containment preparation"
                .to_string(),
        ));
    }
    if record.forced_termination_requested && record.termination_requested.is_none() {
        return Err(ActionError::InvalidJournal(
            "runscript journal claims forced termination without a graceful request"
                .to_string(),
        ));
    }
    if let Some(identity) = record.runtime_identity {
        if identity.device == 0 || identity.inode == 0 {
            return Err(ActionError::InvalidJournal(
                "runscript runtime directory identity is malformed".to_string(),
            ));
        }
    }
    if let Some(descriptor) = record.descriptor.as_ref() {
        if descriptor.token != record.token
            || record.runtime_identity != Some(descriptor.runtime_directory)
        {
            return Err(ActionError::InvalidJournal(
                "runscript containment descriptor has a foreign token or runtime identity"
                    .to_string(),
            ));
        }
    }
    match operation.state {
        OperationState::Prepared => {
            if record.start_committed
                || record.user_code_released
                || record.descriptor.is_some()
                || record.termination_requested.is_some()
                || record.forced_termination_requested
                || record.leader_exit_status.is_some()
                || record.containment_empty.is_some()
                || record.output_capture.is_some()
                || record.terminal.is_some()
            {
                return Err(ActionError::InvalidJournal(
                    "prepared runscript operation contains execution progress".to_string(),
                ));
            }
        }
        OperationState::ScriptStartRecorded => {
            if !record.start_committed
                || record.runtime_identity.is_none()
                || record.descriptor.is_none()
                || record.terminal.is_some()
            {
                return Err(ActionError::InvalidJournal(
                    "started runscript lacks a prepared descriptor or is already terminal"
                        .to_string(),
                ));
            }
        }
        OperationState::ScriptCompleted
        | OperationState::Committed
        | OperationState::CleanupStarted
        | OperationState::CleanupComplete => {
            let output = record.output_capture.as_ref().ok_or_else(|| {
                ActionError::InvalidJournal(
                    "successful runscript omitted output-capture completion".to_string(),
                )
            })?;
            if record.terminal != Some(ScriptTerminalState::Success)
                || !record.start_committed
                || !record.user_code_released
                || record.descriptor.is_none()
                || record.leader_exit_status.is_none()
                || record.containment_empty.is_none()
                || matches!(output.stdout, OutputCaptureTerminal::Abandoned)
                || matches!(output.stderr, OutputCaptureTerminal::Abandoned)
            {
                return Err(ActionError::InvalidJournal(
                    "successful runscript lacks complete containment/output proof".to_string(),
                ));
            }
        }
        OperationState::FailedDeterministic => {
            match record.terminal {
                Some(ScriptTerminalState::SetupFailedBeforeExecution) => {
                    if record.user_code_released {
                        return Err(ActionError::InvalidJournal(
                            "setup-failed runscript claims user code was released".to_string(),
                        ));
                    }
                }
                Some(
                    ScriptTerminalState::ExitFailure
                    | ScriptTerminalState::TimedOut
                    | ScriptTerminalState::BackgroundDescendants,
                ) => {
                    let output = record.output_capture.as_ref().ok_or_else(|| {
                        ActionError::InvalidJournal(
                            "executed failed runscript omitted output-capture completion"
                                .to_string(),
                        )
                    })?;
                    if !record.start_committed
                        || !record.user_code_released
                        || record.descriptor.is_none()
                        || record.leader_exit_status.is_none()
                        || record.containment_empty.is_none()
                        || matches!(output.stdout, OutputCaptureTerminal::Abandoned)
                        || matches!(output.stderr, OutputCaptureTerminal::Abandoned)
                    {
                        return Err(ActionError::InvalidJournal(
                            "executed failed runscript lacks complete containment/output proof"
                                .to_string(),
                        ));
                    }
                }
                _ => {
                    return Err(ActionError::InvalidJournal(
                        "failed runscript has an incompatible terminal classification"
                            .to_string(),
                    ));
                }
            }
        }
        OperationState::ManualRecoveryRequired => {
            if !matches!(
                record.terminal,
                Some(
                    ScriptTerminalState::CancelledAfterStart
                        | ScriptTerminalState::ContainmentUncertain
                        | ScriptTerminalState::ManualRecoveryRequired
                )
            ) {
                return Err(ActionError::InvalidJournal(
                    "manual-recovery runscript has no ambiguous terminal classification"
                        .to_string(),
                ));
            }
        }
        OperationState::CancelledBeforeMutation => {
            if record.user_code_released {
                return Err(ActionError::InvalidJournal(
                    "cancelled-before-mutation runscript released user code".to_string(),
                ));
            }
        }
        _ => {
            return Err(ActionError::InvalidJournal(format!(
                "runscript has impossible operation state {:?}",
                operation.state
            )));
        }
    }
    Ok(())
}

fn validate_plan_identities(plan: &PlannedOperation) -> Result<(), ActionError> {
    match plan {
        PlannedOperation::Rename {
            expected_source,
            expected_staged,
            ..
        } => {
            validate_object_identity(expected_source)?;
            validate_object_identity(expected_staged)
        }
        PlannedOperation::Copy { expected_source, .. }
        | PlannedOperation::Move { expected_source, .. }
        | PlannedOperation::Delete { expected_target: expected_source, .. } => {
            validate_object_identity(expected_source)
        }
        PlannedOperation::RepairCopyMetadata {
            expected_source,
            expected_destination,
            ..
        } => {
            validate_object_identity(expected_source)?;
            validate_object_identity(expected_destination)
        }
        PlannedOperation::RunScript {
            expected_script, ..
        } => validate_object_identity(expected_script),
        PlannedOperation::CreateDirectory { .. } => Ok(()),
    }
}

fn validate_object_identity(identity: &ObjectIdentity) -> Result<(), ActionError> {
    if identity.content_sha256.len() != 64
        || !identity
            .content_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(ActionError::InvalidJournal(
            "object identity contains an invalid SHA-256 digest".to_string(),
        ));
    }
    if !identity.copy_metadata.root.relative_path.as_os_str().is_empty()
        || identity.copy_metadata.root.kind != identity.kind
        || identity.copy_metadata.root.mode & !0o7777 != 0
    {
        return Err(ActionError::InvalidJournal(
            "object identity has malformed root copy metadata".to_string(),
        ));
    }
    let mut previous: Option<&Path> = None;
    for entry in &identity.copy_metadata.descendants {
        validate_relative_metadata_path(&entry.relative_path)?;
        if entry.mode & !0o7777 != 0 {
            return Err(ActionError::InvalidJournal(
                "object identity contains an invalid metadata mode".to_string(),
            ));
        }
        if previous.is_some_and(|path| path >= entry.relative_path.as_path()) {
            return Err(ActionError::InvalidJournal(
                "object identity metadata descendants are not strictly sorted".to_string(),
            ));
        }
        previous = Some(entry.relative_path.as_path());
    }
    match identity.kind {
        ObjectKind::File => {
            if identity.entry_count != 1 || !identity.copy_metadata.descendants.is_empty() {
                return Err(ActionError::InvalidJournal(
                    "file identity must describe exactly one metadata entry".to_string(),
                ));
            }
        }
        ObjectKind::Directory => {
            if identity.entry_count as usize != identity.copy_metadata.descendants.len() {
                return Err(ActionError::InvalidJournal(
                    "directory identity metadata cardinality differs from content identity"
                        .to_string(),
                ));
            }
        }
    }
    Ok(())
}

fn validate_relative_metadata_path(path: &Path) -> Result<(), ActionError> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        return Err(ActionError::InvalidJournal(
            "copy metadata descendant path is empty or absolute".to_string(),
        ));
    }
    for component in path.components() {
        if !matches!(component, std::path::Component::Normal(_)) {
            return Err(ActionError::InvalidJournal(
                "copy metadata descendant path is not normalized".to_string(),
            ));
        }
    }
    Ok(())
}

fn validate_operation_paths(
    operation: &JournalOperation,
    configured: &ConversionAction,
    context: &ActionContext,
    claim_id: &str,
    action_index: usize,
    operation_index: usize,
    batch_member_reader: bool,
) -> Result<(), ActionError> {
    for path in operation.plan.all_paths() {
        validate_mutation_path(path, false)?;
    }
    match (&operation.plan, configured) {
        (
            PlannedOperation::Rename {
                source,
                destination,
                staging,
                expected_source,
                ..
            },
            ConversionAction::Rename(action),
        ) => {
            validate_serialized_target(source, &action.targeting, context)?;
            reject_protected_action_artifact(source, context, "serialized rename source")
                .map_err(|error| ActionError::InvalidJournal(error.to_string()))?;
            reject_protected_source(source, &action.targeting, context)?;
            let expected_destination = rename_destination_for_kind(
                action,
                context,
                source,
                expected_source.kind == ObjectKind::Directory,
            )?;
            if destination != &expected_destination {
                return Err(ActionError::InvalidJournal(format!(
                    "rename destination is not derived from configured semantics: {}",
                    destination.display()
                )));
            }
            validate_target_under_subject(destination, &context.subject_dir)?;
            reject_protected_action_artifact(destination, context, "serialized rename destination")
                .map_err(|error| ActionError::InvalidJournal(error.to_string()))?;
            let expected_root = rename_staging_root(context, claim_id, action_index)?;
            if staging.parent() != Some(expected_root.as_path())
                || staging
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(|name| name.len() == 6 && name.bytes().all(|byte| byte.is_ascii_digit()))
                    != Some(true)
            {
                return Err(ActionError::InvalidJournal(format!(
                    "rename staging path is foreign: {}",
                    staging.display()
                )));
            }
            validate_workspace_path(&expected_root, claim_id)?;
        }
        (
            PlannedOperation::Copy {
                source,
                destination,
                temporary,
                publication_witness,
                ..
            },
            ConversionAction::Copy(action),
        ) => {
            validate_serialized_target(source, &action.targeting, context)?;
            reject_protected_action_artifact(source, context, "serialized copy source")
                .map_err(|error| ActionError::InvalidJournal(error.to_string()))?;
            let destination_root =
                render_action_path(&action.destination, context, &context.subject_dir)?;
            let file_name = source.file_name().ok_or_else(|| {
                ActionError::InvalidJournal("copy source has no file name".to_string())
            })?;
            if destination != &destination_root.join(file_name)
                || temporary
                    != &action_temporary_path(
                        destination,
                        claim_id,
                        action_index,
                        operation_index,
                        "copy",
                    )?
                || publication_witness
                    != &publication_witness_path(
                        destination,
                        claim_id,
                        action_index,
                        operation_index,
                        operation_expected_kind(&operation.plan)?,
                    )?
            {
                return Err(ActionError::InvalidJournal(
                    "copy plan contains foreign destination or recovery paths".to_string(),
                ));
            }
            reject_protected_action_artifact(destination, context, "serialized copy destination")
                .map_err(|error| ActionError::InvalidJournal(error.to_string()))?;
            validate_owned_temporary(temporary, destination.parent(), claim_id)?;
            validate_publication_witness_path(
                publication_witness,
                destination,
                claim_id,
                operation_expected_kind(&operation.plan)?,
            )?;
        }
        (
            PlannedOperation::RepairCopyMetadata {
                source,
                destination,
                expected_source,
                expected_destination,
                include_hidden,
            },
            ConversionAction::Copy(action),
        ) => {
            let exact_target = validate_serialized_target(source, &action.targeting, context)?;
            if *include_hidden != exact_target {
                return Err(ActionError::InvalidJournal(
                    "copy metadata repair hidden-entry policy differs from configured targeting"
                        .to_string(),
                ));
            }
            reject_protected_action_artifact(source, context, "serialized copy metadata source")
                .map_err(|error| ActionError::InvalidJournal(error.to_string()))?;
            let destination_root =
                render_action_path(&action.destination, context, &context.subject_dir)?;
            let file_name = source.file_name().ok_or_else(|| {
                ActionError::InvalidJournal("copy metadata source has no file name".to_string())
            })?;
            if destination != &destination_root.join(file_name)
                || !expected_source.same_content(expected_destination)
                || expected_source.copy_state_equivalent(expected_destination)
            {
                return Err(ActionError::InvalidJournal(
                    "copy metadata repair does not describe a content-equivalent metadata mismatch"
                        .to_string(),
                ));
            }
            reject_protected_action_artifact(
                destination,
                context,
                "serialized copy metadata destination",
            )
            .map_err(|error| ActionError::InvalidJournal(error.to_string()))?;
        }
        (
            PlannedOperation::Move {
                source,
                destination,
                temporary,
                publication_witness,
                source_witness,
                ..
            },
            ConversionAction::Move(action),
        ) => {
            validate_serialized_target(source, &action.targeting, context)?;
            reject_protected_action_artifact(source, context, "serialized move source")
                .map_err(|error| ActionError::InvalidJournal(error.to_string()))?;
            reject_protected_source(source, &action.targeting, context)?;
            let destination_root =
                render_action_path(&action.destination, context, &context.subject_dir)?;
            let file_name = source.file_name().ok_or_else(|| {
                ActionError::InvalidJournal("move source has no file name".to_string())
            })?;
            if destination != &destination_root.join(file_name)
                || temporary
                    != &action_temporary_path(
                        destination,
                        claim_id,
                        action_index,
                        operation_index,
                        "move-copy",
                    )?
                || publication_witness
                    != &publication_witness_path(
                        destination,
                        claim_id,
                        action_index,
                        operation_index,
                        operation_expected_kind(&operation.plan)?,
                    )?
                || source_witness
                    != &same_directory_witness(
                        source,
                        claim_id,
                        action_index,
                        operation_index,
                        "move-source",
                    )?
            {
                return Err(ActionError::InvalidJournal(
                    "move plan contains foreign destination or recovery paths".to_string(),
                ));
            }
            reject_protected_action_artifact(destination, context, "serialized move destination")
                .map_err(|error| ActionError::InvalidJournal(error.to_string()))?;
            validate_owned_temporary(temporary, destination.parent(), claim_id)?;
            validate_publication_witness_path(
                publication_witness,
                destination,
                claim_id,
                operation_expected_kind(&operation.plan)?,
            )?;
            validate_owned_temporary(source_witness, source.parent(), claim_id)?;
        }
        (
            PlannedOperation::Delete {
                target, witness, ..
            },
            ConversionAction::Delete(action),
        ) => {
            validate_serialized_target(target, &action.targeting, context)?;
            reject_protected_action_artifact(target, context, "serialized delete target")
                .map_err(|error| ActionError::InvalidJournal(error.to_string()))?;
            reject_protected_source(target, &action.targeting, context)?;
            if witness
                != &same_directory_witness(
                    target,
                    claim_id,
                    action_index,
                    operation_index,
                    "delete",
                )?
            {
                return Err(ActionError::InvalidJournal(
                    "delete witness path is foreign".to_string(),
                ));
            }
            validate_owned_temporary(witness, target.parent(), claim_id)?;
        }
        (
            PlannedOperation::CreateDirectory { path },
            ConversionAction::CreateFolder(action),
        ) => {
            let expected = render_action_path(&action.path, context, &context.subject_dir)?;
            if path != &expected {
                return Err(ActionError::InvalidJournal(
                    "create_folder path differs from configured rendering".to_string(),
                ));
            }
            reject_protected_action_artifact(path, context, "serialized create_folder destination")
                .map_err(|error| ActionError::InvalidJournal(error.to_string()))?;
        }
        (
            PlannedOperation::RunScript {
                script,
                expected_script,
                args,
                working_directory,
                environment,
                timeout_seconds,
                runtime_directory,
                containment_token,
            },
            ConversionAction::Runscript(action),
        ) => {
            validate_object_identity(expected_script)?;
            if expected_script.kind != ObjectKind::File {
                return Err(ActionError::InvalidJournal(
                    "runscript expected identity is not a regular file".to_string(),
                ));
            }
            if script != &resolve_script_path(&action.script)?
                || args != &action.args
                || working_directory != &context.subject_dir
                || timeout_seconds != &action.timeout_seconds
                || !script_environment_matches(environment, context, batch_member_reader)
                || environment.keys().any(|key| !valid_environment_key(key))
                || runtime_directory
                    != &script_runtime_directory(context, claim_id, action_index, operation_index)?
                || containment_token
                    != &script_containment_token(claim_id, action_index, operation_index)
            {
                return Err(ActionError::InvalidJournal(
                    "runscript operation contains foreign execution or containment data"
                        .to_string(),
                ));
            }
            let record = operation.script_execution.as_ref().ok_or_else(|| {
                ActionError::InvalidJournal(
                    "runscript operation omitted its durable execution record".to_string(),
                )
            })?;
            if record.schema_version != SCRIPT_EXECUTION_SCHEMA_VERSION
                || &record.token != containment_token
                || &record.runtime_directory != runtime_directory
            {
                return Err(ActionError::InvalidJournal(
                    "runscript durable execution identity differs from its plan".to_string(),
                ));
            }
        }
        _ => {
            return Err(ActionError::InvalidJournal(
                "operation kind does not correspond to configured action".to_string(),
            ));
        }
    }
    Ok(())
}

fn validate_serialized_target(
    path: &Path,
    spec: &TargetSpec,
    context: &ActionContext,
) -> Result<bool, ActionError> {
    validate_target_patterns(spec)?;
    validate_target_under_subject(path, &context.subject_dir)?;
    if path_intersects_reserved_action_authority(path, context)? {
        return Err(ActionError::InvalidJournal(format!(
            "serialized target intersects Tonepoet recovery authority: {}",
            path.display()
        )));
    }
    let relative = path.strip_prefix(&context.subject_dir).map_err(|_| {
        ActionError::InvalidJournal(format!(
            "serialized target is outside subject: {}",
            path.display()
        ))
    })?;
    let name = path
        .file_name()
        .map(|value| value.to_string_lossy())
        .ok_or_else(|| ActionError::InvalidJournal("serialized target has no name".to_string()))?;
    let exact = spec
        .target
        .iter()
        .filter(|pattern| !contains_wildcard(pattern))
        .map(|pattern| checked_relative_target(pattern))
        .collect::<Result<Vec<_>, _>>()?
        .iter()
        .any(|candidate| candidate == relative);
    let wildcard = !relative.components().any(|component| {
        matches!(component, Component::Normal(value) if is_hidden_name(value))
    }) && spec.target.iter().any(|pattern| {
        contains_wildcard(pattern) && (context.semantics.wildcard_matches)(pattern, &name)
    });
    if !exact && !wildcard {
        return Err(ActionError::InvalidJournal(format!(
            "serialized target is not authorized by configured include patterns: {}",
            path.display()
        )));
    }
    let excluded = spec.exclude.iter().any(|pattern| {
        if contains_wildcard(pattern) {
            (context.semantics.wildcard_matches)(pattern, &name)
        } else {
            pattern.eq_ignore_ascii_case(&name)
        }
    });
    if excluded || (!exact && protected_generated_wildcard_match(path, context)) {
        return Err(ActionError::InvalidJournal(format!(
            "serialized target is excluded or protected: {}",
            path.display()
        )));
    }
    Ok(exact)
}

fn validate_workspace_path(path: &Path, claim_id: &str) -> Result<(), ActionError> {
    validate_mutation_path(path, false)?;
    if !is_action_owned_path(path, claim_id) {
        return Err(ActionError::InvalidJournal(format!(
            "journal workspace path is not owned by claim {claim_id}: {}",
            path.display()
        )));
    }
    Ok(())
}

fn validate_owned_temporary(
    path: &Path,
    expected_parent: Option<&Path>,
    claim_id: &str,
) -> Result<(), ActionError> {
    validate_workspace_path(path, claim_id)?;
    if path.parent() != expected_parent {
        return Err(ActionError::InvalidJournal(format!(
            "temporary/witness path is not in the required same directory: {}",
            path.display()
        )));
    }
    Ok(())
}

fn authorized_workspace_paths(journal: &ActionJournal) -> BTreeSet<PathBuf> {
    let mut paths = BTreeSet::new();
    for action in &journal.actions {
        for operation in &action.operations {
            match &operation.plan {
                PlannedOperation::Rename { staging, .. } => {
                    if let Some(parent) = staging.parent() {
                        paths.insert(parent.to_path_buf());
                    }
                }
                PlannedOperation::Copy {
                    temporary,
                    publication_witness,
                    ..
                } => {
                    paths.insert(temporary.clone());
                    paths.insert(publication_witness.clone());
                }
                PlannedOperation::RepairCopyMetadata { .. } => {}
                PlannedOperation::Move {
                    temporary,
                    publication_witness,
                    source_witness,
                    ..
                } => {
                    paths.insert(temporary.clone());
                    paths.insert(publication_witness.clone());
                    paths.insert(source_witness.clone());
                }
                PlannedOperation::Delete { witness, .. } => {
                    paths.insert(witness.clone());
                }
                PlannedOperation::CreateDirectory { .. } => {}
                PlannedOperation::RunScript { runtime_directory, .. } => {
                    paths.insert(runtime_directory.clone());
                }
            }
        }
    }
    paths
}

pub fn workspace_has_unresolved_action_state(path: &Path) -> bool {
    workspace_has_unresolved_action_state_inner(path, 0)
}

fn prune_action_journal_retention_best_effort(context: &ActionContext) {
    prune_terminal_action_journals_best_effort(
        &context.journal_dir,
        TERMINAL_JOURNAL_RETENTION_COUNT,
    );
    let non_batch_root = context.output_root.join(".tonepoet-actions");
    if context.journal_dir.starts_with(&non_batch_root) {
        prune_terminal_action_journals_best_effort(
            &non_batch_root,
            TERMINAL_JOURNAL_RETENTION_COUNT,
        );
    }
}

#[derive(Debug)]
struct TerminalJournalRetentionCandidate {
    modified: SystemTime,
    journal_path: PathBuf,
    result_path: Option<PathBuf>,
}

/// Probe an entry without collapsing permission, I/O, or metadata failures
/// into "absent". `Path::exists` deliberately does that collapse, which is
/// unacceptable anywhere absence grants cleanup or election authority.
fn path_entry_exists(path: &Path) -> Result<bool, ActionError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}


fn read_regular_file_optional_no_follow(path: &Path) -> Result<Option<Vec<u8>>, ActionError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(ActionError::Conflict(format!(
                "durable authority is not a regular no-follow file: {}",
                path.display()
            )));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(ActionError::Conflict(format!(
            "durable authority changed type while opening: {}",
            path.display()
        )));
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(Some(bytes))
}

fn write_bytes_create_new_durable(path: &Path, bytes: &[u8]) -> Result<(), ActionError> {
    let parent = path.parent().ok_or_else(|| {
        ActionError::UnsafePath(format!("durable file has no parent: {}", path.display()))
    })?;
    create_dir_all_no_symlink(parent)?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    sync_parent(path)
}

/// Test fixture writer: same serialization + durability as the production
/// claim/journal writers (`serde_json::to_vec_pretty` + create-new durable).
#[cfg(test)]
fn write_json_create_new_durable<T: serde::Serialize>(
    path: &Path,
    value: &T,
) -> Result<(), ActionError> {
    let bytes = serde_json::to_vec_pretty(value).map_err(ActionError::Serialization)?;
    write_bytes_create_new_durable(path, &bytes)
}

fn sync_parent(path: &Path) -> Result<(), ActionError> {
    let parent = path.parent().ok_or_else(|| {
        ActionError::UnsafePath(format!("path has no parent to sync: {}", path.display()))
    })?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

fn revalidate_exact_file(path: &Path, expected: &[u8]) -> Result<(), ActionError> {
    let current = read_regular_file_optional_no_follow(path)?.ok_or_else(|| {
        ActionError::Contradiction(format!(
            "durable authority disappeared during revalidation: {}",
            path.display()
        ))
    })?;
    if current != expected {
        return Err(ActionError::Contradiction(format!(
            "durable authority changed during revalidation: {}",
            path.display()
        )));
    }
    Ok(())
}

/// Bounded, fail-closed retention for terminal action authority. Only journals
/// that are current-schema, self-consistent, cleanup-complete, and free of any
/// recovery requirement are eligible. If an election result exists, it must
/// validate against the exact journal identity and report before either file is
/// removed. Unreadable, unknown, contradictory, or non-terminal state remains
/// untouched. At most `keep` recent terminal authorities survive indefinitely;
/// older authorities are also retired after the age bound.
pub fn prune_terminal_action_journals_best_effort(root: &Path, keep: usize) {
    let mut terminal = Vec::<TerminalJournalRetentionCandidate>::new();
    collect_terminal_action_journals(root, 0, &mut terminal);
    terminal.sort_by(|left, right| {
        right
            .modified
            .cmp(&left.modified)
            .then_with(|| right.journal_path.cmp(&left.journal_path))
    });
    let now = SystemTime::now();
    for (index, candidate) in terminal.into_iter().enumerate() {
        let expired = now
            .duration_since(candidate.modified)
            .map(|age| age >= TERMINAL_JOURNAL_RETENTION_AGE)
            .unwrap_or(false);
        if index < keep && !expired {
            continue;
        }

        // Remove the redundant election result first. If that fails, retain
        // the journal as the durable terminal authority rather than creating
        // an ambiguous partial-retention state.
        if let Some(result_path) = candidate.result_path.as_ref() {
            match fs::remove_file(result_path) {
                Ok(()) => {
                    if let Err(error) = sync_parent(result_path) {
                        log::warn!(
                            "terminal action retention removed {} but could not sync its parent: {error}",
                            result_path.display()
                        );
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => {
                    log::warn!(
                        "terminal action retention could not remove validated result {}: {error}; preserving journal {}",
                        result_path.display(),
                        candidate.journal_path.display()
                    );
                    continue;
                }
            }
        }

        match fs::remove_file(&candidate.journal_path) {
            Ok(()) => {
                if let Err(error) = sync_parent(&candidate.journal_path) {
                    log::warn!(
                        "terminal action retention removed {} but could not sync its parent: {error}",
                        candidate.journal_path.display()
                    );
                }
                remove_empty_action_authority_dirs_best_effort(&candidate.journal_path);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => log::warn!(
                "terminal action retention could not remove {}: {error}",
                candidate.journal_path.display()
            ),
        }
    }
}

fn remove_empty_action_authority_dirs_best_effort(journal_path: &Path) {
    let Some(journal_dir) = journal_path.parent() else {
        return;
    };
    match fs::remove_dir(journal_dir) {
        Ok(()) => {
            if let Err(error) = sync_parent(journal_dir) {
                log::warn!(
                    "terminal action retention removed empty directory {} but could not sync its parent: {error}",
                    journal_dir.display()
                );
            }
        }
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::DirectoryNotEmpty
            ) => {}
        Err(error) => log::warn!(
            "terminal action retention could not remove empty journal directory {}: {error}",
            journal_dir.display()
        ),
    }

    if journal_dir.file_name().and_then(|name| name.to_str())
        != Some(".tonepoet-action-journals")
    {
        return;
    }
    let Some(coordination_dir) = journal_dir.parent() else {
        return;
    };
    match fs::remove_dir(coordination_dir) {
        Ok(()) => {
            if let Err(error) = sync_parent(coordination_dir) {
                log::warn!(
                    "terminal action retention removed empty coordination directory {} but could not sync its parent: {error}",
                    coordination_dir.display()
                );
            }
        }
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::DirectoryNotEmpty
            ) => {}
        Err(error) => log::warn!(
            "terminal action retention could not remove empty coordination directory {}: {error}",
            coordination_dir.display()
        ),
    }
}

fn collect_terminal_action_journals(
    root: &Path,
    depth: usize,
    terminal: &mut Vec<TerminalJournalRetentionCandidate>,
) {
    if depth > 8 {
        return;
    }
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return,
        Err(error) => {
            log::warn!(
                "terminal action retention could not scan {}: {error}",
                root.display()
            );
            return;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                log::warn!(
                    "terminal action retention encountered an unreadable entry under {}: {error}",
                    root.display()
                );
                return;
            }
        };
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) => {
                log::warn!(
                    "terminal action retention could not inspect {}: {error}",
                    entry.path().display()
                );
                return;
            }
        };
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            collect_terminal_action_journals(&entry.path(), depth + 1, terminal);
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("actions-") || !name.ends_with(".journal.json") {
            continue;
        }
        // The pruner may be invoked through a live descriptor route; journal
        // authority comparisons and removals use the STABLE path.
        let Some(journal_path) = resolve_descriptor_route_to_stable(&entry.path()) else {
            continue;
        };
        let bytes = match fs::read(&journal_path) {
            Ok(bytes) => bytes,
            Err(_) => continue,
        };
        let journal = match deserialize_action_journal(&bytes) {
            Ok(journal) => journal,
            Err(_) => continue,
        };
        if validate_resolved_terminal_journal_authority(&journal, &journal_path).is_err() {
            continue;
        }
        let terminal_result = journal
            .terminal
            .as_ref()
            .expect("resolved terminal journal has a terminal report");

        let election = ActionElectionIdentity {
            run_identity: journal.run_identity.clone(),
            album_identity: journal.album_identity.clone(),
            phase: journal.phase,
            pipeline_serialized: journal.pipeline_serialized.clone(),
            pipeline_sha256: journal.pipeline_sha256.clone(),
        };
        if validate_election_identity(&election).is_err() {
            continue;
        }
        let stem = election_file_stem(&election);
        let expected_journal_name = format!("{stem}.journal.json");
        if name.as_ref() != expected_journal_name.as_str() {
            continue;
        }
        let live_claim = journal_path.with_file_name(format!("{stem}.claim.json"));
        let parent_claim = journal_path
            .parent()
            .and_then(Path::parent)
            .map(|parent| parent.join(format!("{stem}.claim.json")));
        let live_claim_present = match path_entry_exists(&live_claim) {
            Ok(present) => present,
            Err(_) => continue,
        };
        let parent_claim_present = match parent_claim.as_ref() {
            Some(path) => match path_entry_exists(path) {
                Ok(present) => present,
                Err(_) => continue,
            },
            None => false,
        };
        if live_claim_present || parent_claim_present {
            continue;
        }

        let local_result = journal_path.with_file_name(format!("{stem}.result.json"));
        let parent_result = journal_path
            .parent()
            .and_then(Path::parent)
            .map(|parent| parent.join(format!("{stem}.result.json")));
        let mut existing_results = Vec::new();
        let mut result_probe_failed = false;
        for path in [Some(local_result), parent_result].into_iter().flatten() {
            match path_entry_exists(&path) {
                Ok(true) => existing_results.push(path),
                Ok(false) => {}
                Err(_) => {
                    result_probe_failed = true;
                    break;
                }
            }
        }
        if result_probe_failed {
            continue;
        }
        if existing_results.len() > 1 {
            continue;
        }
        let result_path = existing_results.into_iter().next();
        if let Some(path) = result_path.as_ref() {
            let result_bytes = match fs::read(path) {
                Ok(bytes) => bytes,
                Err(_) => continue,
            };
            let result = match serde_json::from_slice::<ActionResultRecord>(&result_bytes) {
                Ok(result) => result,
                Err(_) => continue,
            };
            if result.election != election
                || result.report != terminal_result.report
                || read_valid_result(path, &election, None)
                    .ok()
                    .as_ref()
                    != Some(&terminal_result.report)
            {
                continue;
            }
        }

        let modified = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .unwrap_or(UNIX_EPOCH);
        terminal.push(TerminalJournalRetentionCandidate {
            modified,
            journal_path,
            result_path,
        });
    }
}

/// Resolve a live descriptor-namespace route (`/proc/self/fd/N/rest`) to its
/// stable filesystem path by reading the fd link — valid exactly while the
/// descriptor is open, which record-writing guarantees. Non-routed paths
/// return unchanged; a route that cannot be resolved returns None so callers
/// fail closed instead of persisting a route that dies with the descriptor.
pub(crate) fn resolve_descriptor_route_to_stable(path: &Path) -> Option<PathBuf> {
    if !path.starts_with("/proc/self/fd") && !path.starts_with("/dev/fd") {
        return Some(path.to_path_buf());
    }
    // Route anchor is /proc/self/fd/<N> (5 components counting the root) or
    // /dev/fd/<N> (4). Read the live fd link and graft the remainder on.
    let anchor_len = if path.starts_with("/proc/self/fd") { 5 } else { 4 };
    let mut components = path.components();
    let mut anchor = PathBuf::new();
    for _ in 0..anchor_len {
        anchor.push(components.next()?.as_os_str());
    }
    let target = std::fs::read_link(&anchor).ok()?;
    if !target.is_absolute() {
        return None;
    }
    let mut resolved = target;
    for rest in components {
        resolved.push(rest.as_os_str());
    }
    Some(resolved)
}

fn workspace_has_unresolved_action_state_inner(path: &Path, depth: usize) -> bool {
    // Coordination trees are shallow in normal operation. The cap protects a
    // generic stale sweeper from adversarially deep directory structures while
    // still finding action state below per-run/per-album directories.
    if depth > 32 {
        return true;
    }
    let inside_reserved_action_authority = path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            name == ".tonepoet-action-journals"
                || name == ".tonepoet-actions-manual"
                || name == ".tonepoet-actions"
        });
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return false,
        Err(_) => {
            // This predicate is a deletion veto. An absent authority root is
            // empty; every other inability to inspect it must fail closed.
            return true;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => return true,
        };
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => return true,
        };
        let action_authority_name = name.starts_with("actions-")
            || name.starts_with(INTERNAL_WORKSPACE_PREFIX)
            || name.contains("action-publish-witness")
            || name.contains("action-move-source")
            || name.contains("action-delete");
        if file_type.is_symlink() && action_authority_name {
            return true;
        }

        if name.starts_with("actions-") && name.ends_with(".claim.json") {
            if name.contains(".stale-") {
                let bytes = match fs::read(entry.path()) {
                    Ok(bytes) => bytes,
                    Err(_) => return true,
                };
                let claim = match serde_json::from_slice::<ActionClaimRecord>(&bytes) {
                    Ok(claim) => claim,
                    Err(_) => return true,
                };
                if validate_owner_identity(&claim.owner).is_err() {
                    return true;
                }
                match owner_liveness(&claim.owner) {
                    Ok(OwnerLiveness::DeadLocal) => continue,
                    Ok(OwnerLiveness::Alive | OwnerLiveness::RemoteOrUnknown) | Err(_) => {
                        return true;
                    }
                }
            }
            return true;
        }
        if name.starts_with("actions-") && name.ends_with(".write-tmp") {
            return true;
        }
        if name.starts_with("actions-") && name.ends_with(".journal.json") {
            let bytes = match fs::read(entry.path()) {
                Ok(bytes) => bytes,
                Err(_) => return true,
            };
            let journal = match deserialize_action_journal(&bytes) {
                Ok(journal) => journal,
                Err(_) => return true,
            };
            // Cleanup may scan through a live descriptor route; the journal
            // records its STABLE path, so authority comparison must use it.
            let Some(stable_path) = resolve_descriptor_route_to_stable(&entry.path()) else {
                return true;
            };
            if validate_resolved_terminal_journal_authority(&journal, &stable_path).is_err() {
                return true;
            }
            continue;
        }
        if name.starts_with("actions-") && name.ends_with(".result.json") {
            let bytes = match fs::read(entry.path()) {
                Ok(bytes) => bytes,
                Err(_) => return true,
            };
            let result = match serde_json::from_slice::<ActionResultRecord>(&bytes) {
                Ok(result) => result,
                Err(_) => return true,
            };
            let Some(stable_result_path) = resolve_descriptor_route_to_stable(&entry.path()) else {
                return true;
            };
            let report = match read_valid_result(&stable_result_path, &result.election, None) {
                Ok(report) => report,
                Err(_) => return true,
            };
            if report.recovery_required {
                return true;
            }
            continue;
        }
        if name.starts_with(INTERNAL_WORKSPACE_PREFIX)
            || name.contains("action-publish-witness")
            || name.contains("action-move-source")
            || name.contains("action-delete")
        {
            return true;
        }
        if inside_reserved_action_authority && name == ".manual-run.lock" {
            if file_type.is_file() {
                continue;
            }
            return true;
        }
        if inside_reserved_action_authority {
            // Reserved authority directories are forward-compatible deletion
            // vetoes: only artifacts that the current build can positively
            // validate as resolved terminal state are ignorable. An unknown
            // file, symlink, or subdirectory may be recovery authority from a
            // newer build and must never be recursively discarded.
            return true;
        }
        let is_directory = file_type.is_dir() && !file_type.is_symlink();
        if is_directory
            && workspace_has_unresolved_action_state_inner(&entry.path(), depth + 1)
        {
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Album-phase election
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionElectionIdentity {
    pub run_identity: String,
    pub album_identity: String,
    pub phase: ActionPhase,
    pub pipeline_serialized: String,
    pub pipeline_sha256: String,
}

impl ActionElectionIdentity {
    pub fn new(
        run_identity: impl Into<String>,
        album_identity: impl Into<String>,
        phase: ActionPhase,
        pipeline: &ActionPipeline,
    ) -> Result<Self, ActionError> {
        let pipeline_serialized = pipeline.canonical_serialization()?;
        let pipeline_sha256 = sha256_hex(pipeline_serialized.as_bytes());
        Ok(Self {
            run_identity: run_identity.into(),
            album_identity: album_identity.into(),
            phase,
            pipeline_serialized,
            pipeline_sha256,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ProcessOwnerIdentity {
    pub(crate) machine_identity: String,
    pub(crate) host_identity: String,
    pub(crate) boot_identity: String,
    pub(crate) pid: u32,
    pub(crate) process_start_identity: String,
    pub(crate) claim_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ActionClaimRecord {
    schema_version: u32,
    election: ActionElectionIdentity,
    owner: ProcessOwnerIdentity,
    created_unix_nanos: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ActionResultRecord {
    schema_version: u32,
    election: ActionElectionIdentity,
    claim: ActionClaimRecord,
    claim_sha256: String,
    report: ActionPhaseReport,
    completed_unix_nanos: u64,
}

#[derive(Debug)]
pub enum ActionElection {
    Runner(ActionClaimGuard),
    Wait(ActionWaitHandle),
    Complete(ActionPhaseReport),
}

#[derive(Debug)]
pub struct ActionClaimGuard {
    claim_path: PathBuf,
    result_path: PathBuf,
    claim_bytes: Vec<u8>,
    record: ActionClaimRecord,
    finished: bool,
}

#[derive(Debug, Clone)]
pub struct ActionWaitHandle {
    claim_path: PathBuf,
    result_path: PathBuf,
    election: ActionElectionIdentity,
}

impl ActionClaimGuard {
    pub fn claim_id(&self) -> &str {
        &self.record.owner.claim_id
    }

    pub fn finish(mut self, report: &ActionPhaseReport) -> Result<(), ActionError> {
        validate_election_report(report, &self.record.election)?;
        let result = ActionResultRecord {
            schema_version: RESULT_SCHEMA_VERSION,
            election: self.record.election.clone(),
            claim: self.record.clone(),
            claim_sha256: sha256_hex(&self.claim_bytes),
            report: report.clone(),
            completed_unix_nanos: now_unix_nanos(),
        };
        let result_bytes = serde_json::to_vec_pretty(&result)
            .map_err(ActionError::Serialization)?;

        // The caller holds the album coordination lock while finishing. Validate
        // ownership before publishing the terminal result, then validate again
        // immediately before removing the claim. If the claim changed in the
        // narrow publication window, retract only our exact result and fail
        // closed rather than leaving a result that appears authoritative.
        revalidate_exact_file(&self.claim_path, &self.claim_bytes)?;
        write_bytes_create_new_durable(&self.result_path, &result_bytes)?;
        if let Err(error) = revalidate_exact_file(&self.claim_path, &self.claim_bytes) {
            if revalidate_exact_file(&self.result_path, &result_bytes).is_ok() {
                fs::remove_file(&self.result_path)?;
                sync_parent(&self.result_path)?;
            }
            return Err(error);
        }
        fs::remove_file(&self.claim_path)?;
        sync_parent(&self.claim_path)?;
        self.finished = true;
        release_current_process_owner_claim(&self.record.owner.claim_id);
        Ok(())
    }

    /// Relinquish this exact live-owner claim without publishing a terminal
    /// result. The validated journal remains authoritative and a later elected
    /// runner may resume it. This is used only for recoverable interruption;
    /// deterministic or manual-recovery outcomes are finalized instead.
    pub fn release_for_recovery(mut self) -> Result<(), ActionError> {
        revalidate_exact_file(&self.claim_path, &self.claim_bytes)?;
        fs::remove_file(&self.claim_path)?;
        sync_parent(&self.claim_path)?;
        self.finished = true;
        release_current_process_owner_claim(&self.record.owner.claim_id);
        Ok(())
    }
}

impl Drop for ActionClaimGuard {
    fn drop(&mut self) {
        // A dropped runner intentionally leaves the exact claim in place. A
        // later election under the album lock may reclaim a provably dead local
        // owner; unknown remote ownership always fails closed.
    }
}

impl ActionWaitHandle {
    pub fn wait(
        &self,
        cancellation: &dyn ActionCancellation,
        poll_interval: Duration,
    ) -> Result<ActionPhaseReport, ActionError> {
        loop {
            if cancellation.is_cancelled() {
                return Err(ActionError::CancelledBeforeMutation(
                    "cancelled while waiting for another action runner; owner state was not changed"
                        .to_string(),
                ));
            }
            if path_entry_exists(&self.result_path)? {
                let claim_bytes = match fs::read(&self.claim_path) {
                    Ok(bytes) => Some(bytes),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                    Err(error) => return Err(error.into()),
                };
                return read_valid_result(
                    &self.result_path,
                    &self.election,
                    claim_bytes.as_deref(),
                );
            }
            if !path_entry_exists(&self.claim_path)? {
                return Err(ActionError::Election(
                    "action claim disappeared without a durable result; re-election is required"
                        .to_string(),
                ));
            }
            let claim = read_valid_claim(&self.claim_path, &self.election)?;
            match owner_liveness(&claim.owner)? {
                OwnerLiveness::Alive => {}
                OwnerLiveness::DeadLocal => {
                    return Err(ActionError::Election(
                        "local action owner is dead; caller must re-elect while holding the album lock"
                            .to_string(),
                    ));
                }
                OwnerLiveness::RemoteOrUnknown => {
                    return Err(ActionError::Election(format!(
                        "action claim belongs to another/unknown host and cannot be declared stale: {}",
                        self.claim_path.display()
                    )));
                }
            }
            thread::sleep(poll_interval.max(Duration::from_millis(10)));
        }
    }
}

pub fn elect_action_runner(
    coordination_dir: &Path,
    election: &ActionElectionIdentity,
    allow_proven_dead_local_takeover: bool,
) -> Result<ActionElection, ActionError> {
    validate_election_identity(election)?;
    create_dir_all_no_symlink(coordination_dir)?;
    let stem = election_file_stem(election);
    let claim_path = coordination_dir.join(format!("{stem}.claim.json"));
    let result_path = coordination_dir.join(format!("{stem}.result.json"));

    if path_entry_exists(&result_path)? {
        let claim_bytes = match fs::read(&claim_path) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        return Ok(ActionElection::Complete(read_valid_result(
            &result_path,
            election,
            claim_bytes.as_deref(),
        )?));
    }

    if path_entry_exists(&claim_path)? {
        let existing_bytes = fs::read(&claim_path)?;
        let existing = read_valid_claim_bytes(&existing_bytes, election)?;
        match owner_liveness(&existing.owner)? {
            OwnerLiveness::Alive => {
                return Ok(ActionElection::Wait(ActionWaitHandle {
                    claim_path,
                    result_path,
                    election: election.clone(),
                }));
            }
            OwnerLiveness::RemoteOrUnknown => {
                return Err(ActionError::Election(format!(
                    "remote/unknown owner claim fails closed: {}",
                    claim_path.display()
                )));
            }
            OwnerLiveness::DeadLocal if !allow_proven_dead_local_takeover => {
                return Err(ActionError::Election(
                    "dead local owner requires re-election under the album lock".to_string(),
                ));
            }
            OwnerLiveness::DeadLocal => {
                revalidate_exact_file(&claim_path, &existing_bytes)?;
                let stale_name = coordination_dir.join(format!(
                    "{stem}.stale-{}.claim.json",
                    existing.owner.claim_id
                ));
                rename_path_no_clobber(&claim_path, &stale_name)?;
                sync_parent(&claim_path)?;
            }
        }
    }

    let owner = current_process_owner()?;
    let record = ActionClaimRecord {
        schema_version: CLAIM_SCHEMA_VERSION,
        election: election.clone(),
        owner,
        created_unix_nanos: now_unix_nanos(),
    };
    let claim_bytes = match serde_json::to_vec_pretty(&record) {
        Ok(bytes) => bytes,
        Err(error) => {
            release_current_process_owner_claim(&record.owner.claim_id);
            return Err(ActionError::Serialization(error));
        }
    };
    if let Err(error) = write_bytes_create_new_durable(&claim_path, &claim_bytes) {
        release_current_process_owner_claim(&record.owner.claim_id);
        return Err(error);
    }
    Ok(ActionElection::Runner(ActionClaimGuard {
        claim_path,
        result_path,
        claim_bytes,
        record,
        finished: false,
    }))
}

fn validate_election_identity(election: &ActionElectionIdentity) -> Result<(), ActionError> {
    if election.run_identity.is_empty() || election.album_identity.is_empty() {
        return Err(ActionError::Election(
            "run and album election identities must be non-empty".to_string(),
        ));
    }
    if sha256_hex(election.pipeline_serialized.as_bytes()) != election.pipeline_sha256 {
        return Err(ActionError::Election(
            "pipeline election digest does not match complete serialized identity".to_string(),
        ));
    }
    let pipeline: ActionPipeline = serde_json::from_str(&election.pipeline_serialized)
        .map_err(|error| {
            ActionError::Election(format!(
                "pipeline election identity cannot be deserialized: {error}"
            ))
        })?;
    if pipeline.canonical_serialization()? != election.pipeline_serialized {
        return Err(ActionError::Election(
            "pipeline election identity is not canonically serialized".to_string(),
        ));
    }
    Ok(())
}

fn election_file_stem(election: &ActionElectionIdentity) -> String {
    let mut hasher = Sha256::new();
    hasher.update(election.run_identity.as_bytes());
    hasher.update([0]);
    hasher.update(election.album_identity.as_bytes());
    hasher.update([0]);
    hasher.update(election.phase.as_str().as_bytes());
    hasher.update([0]);
    hasher.update(election.pipeline_sha256.as_bytes());
    format!("actions-{}-{}", election.phase.as_str(), hex::encode(hasher.finalize()))
}

fn read_valid_claim(
    path: &Path,
    election: &ActionElectionIdentity,
) -> Result<ActionClaimRecord, ActionError> {
    read_valid_claim_bytes(&fs::read(path)?, election)
}

fn read_valid_claim_bytes(
    bytes: &[u8],
    election: &ActionElectionIdentity,
) -> Result<ActionClaimRecord, ActionError> {
    let record: ActionClaimRecord =
        serde_json::from_slice(bytes).map_err(ActionError::Serialization)?;
    if record.schema_version != CLAIM_SCHEMA_VERSION || record.election != *election {
        return Err(ActionError::Election(
            "claim schema or election identity mismatch".to_string(),
        ));
    }
    validate_owner_identity(&record.owner)?;
    if record.created_unix_nanos == 0 {
        return Err(ActionError::Election(
            "claim creation timestamp is missing".to_string(),
        ));
    }
    Ok(record)
}

fn read_valid_result(
    path: &Path,
    election: &ActionElectionIdentity,
    claim_bytes: Option<&[u8]>,
) -> Result<ActionPhaseReport, ActionError> {
    read_valid_result_bytes(&fs::read(path)?, election, claim_bytes)
}

fn read_valid_result_bytes(
    bytes: &[u8],
    election: &ActionElectionIdentity,
    claim_bytes: Option<&[u8]>,
) -> Result<ActionPhaseReport, ActionError> {
    let record: ActionResultRecord =
        serde_json::from_slice(bytes).map_err(ActionError::Serialization)?;
    if record.schema_version != RESULT_SCHEMA_VERSION || record.election != *election {
        return Err(ActionError::Election(
            "result schema or election identity mismatch".to_string(),
        ));
    }
    if record.claim.election != *election
        || record.claim.schema_version != CLAIM_SCHEMA_VERSION
    {
        return Err(ActionError::Election(
            "terminal result embeds a foreign or unsupported claim".to_string(),
        ));
    }
    validate_owner_identity(&record.claim.owner)?;
    let embedded_claim_bytes = serde_json::to_vec_pretty(&record.claim)
        .map_err(ActionError::Serialization)?;
    if record.completed_unix_nanos == 0
        || record.claim_sha256.len() != 64
        || !record
            .claim_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || record.claim_sha256 != sha256_hex(&embedded_claim_bytes)
    {
        return Err(ActionError::Election(
            "result timestamp or exact claim binding is malformed".to_string(),
        ));
    }
    if let Some(claim_bytes) = claim_bytes {
        let claim = read_valid_claim_bytes(claim_bytes, election)?;
        if record.claim != claim || record.claim_sha256 != sha256_hex(claim_bytes) {
            return Err(ActionError::Election(
                "terminal result does not belong to the exact surviving claim".to_string(),
            ));
        }
    }
    validate_election_report(&record.report, election)?;
    Ok(record.report)
}

fn validate_election_report(
    report: &ActionPhaseReport,
    election: &ActionElectionIdentity,
) -> Result<(), ActionError> {
    if report.phase != Some(election.phase) {
        return Err(ActionError::Election(
            "result report phase differs from election phase".to_string(),
        ));
    }
    let pipeline: ActionPipeline = serde_json::from_str(&election.pipeline_serialized)
        .map_err(|error| {
            ActionError::Election(format!(
                "election pipeline identity is not a valid action pipeline: {error}"
            ))
        })?;
    if pipeline.canonical_serialization()? != election.pipeline_serialized {
        return Err(ActionError::Election(
            "election pipeline identity is not canonically serialized".to_string(),
        ));
    }
    let configured = pipeline.for_phase(election.phase);
    if report.actions.len() != configured.len() {
        return Err(ActionError::Election(format!(
            "result action count {} differs from configured count {}",
            report.actions.len(),
            configured.len()
        )));
    }

    let mut operation_ids = BTreeSet::new();
    for (index, (action, expected)) in report.actions.iter().zip(configured).enumerate() {
        if action.index != index || action.kind != expected.kind_name() {
            return Err(ActionError::Election(format!(
                "result action slot {} does not match configured action identity",
                index + 1
            )));
        }
        let needs_error = matches!(
            action.status,
            ActionResultStatus::Failed
                | ActionResultStatus::SkippedAfterFailure
                | ActionResultStatus::CancelledBeforeMutation
                | ActionResultStatus::Interrupted
                | ActionResultStatus::ManualRecoveryRequired
        );
        if needs_error != action.error.is_some() {
            return Err(ActionError::Election(format!(
                "result action {} status/error fields are contradictory",
                index + 1
            )));
        }
        match action.status {
            ActionResultStatus::Completed => {
                if action.operations.is_empty()
                    || action
                        .operations
                        .iter()
                        .any(|operation| operation.status != OperationResultStatus::Completed)
                {
                    return Err(ActionError::Election(format!(
                        "completed action {} lacks an all-completed operation result set",
                        index + 1
                    )));
                }
            }
            ActionResultStatus::NoOp if !action.operations.is_empty() => {
                return Err(ActionError::Election(format!(
                    "no-op action {} contains operation results",
                    index + 1
                )));
            }
            ActionResultStatus::SkippedAfterFailure
                if action
                    .operations
                    .iter()
                    .any(|operation| operation.status != OperationResultStatus::Skipped) =>
            {
                return Err(ActionError::Election(format!(
                    "skipped action {} contains a non-skipped operation result",
                    index + 1
                )));
            }
            ActionResultStatus::CancelledBeforeMutation
                if action.operations.iter().any(|operation| {
                    !matches!(
                        operation.status,
                        OperationResultStatus::Skipped | OperationResultStatus::Interrupted
                    )
                }) =>
            {
                return Err(ActionError::Election(format!(
                    "cancelled-before-mutation action {} contains a mutated result",
                    index + 1
                )));
            }
            _ => {}
        }
        for operation in &action.operations {
            if operation.operation_id.trim().is_empty()
                || !operation_ids.insert(operation.operation_id.clone())
            {
                return Err(ActionError::Election(
                    "result contains an empty or duplicate operation identity".to_string(),
                ));
            }
        }
    }

    let has_recovery_state = report.actions.iter().any(|action| {
        matches!(
            action.status,
            ActionResultStatus::Interrupted | ActionResultStatus::ManualRecoveryRequired
        )
    });
    if report.recovery_required != has_recovery_state {
        return Err(ActionError::Election(
            "result recovery flag does not correspond to action states".to_string(),
        ));
    }
    let has_cancelled_state = report.actions.iter().any(|action| {
        matches!(
            action.status,
            ActionResultStatus::CancelledBeforeMutation | ActionResultStatus::Interrupted
        )
    });
    if report.cancelled && !has_cancelled_state {
        return Err(ActionError::Election(
            "result cancellation flag has no cancelled/interrupted action".to_string(),
        ));
    }
    Ok(())
}

fn process_claim_registry() -> &'static Mutex<BTreeSet<String>> {
    static CLAIMS: OnceLock<Mutex<BTreeSet<String>>> = OnceLock::new();
    CLAIMS.get_or_init(|| Mutex::new(BTreeSet::new()))
}

pub(crate) fn process_claim_is_registered(claim_id: &str) -> bool {
    process_claim_registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .contains(claim_id)
}

pub(crate) fn release_current_process_owner_claim(claim_id: &str) {
    process_claim_registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(claim_id);
}

fn process_owner_identity(register_claim: bool) -> ProcessOwnerIdentity {
    let host = current_host_boot_identity();
    let owner = ProcessOwnerIdentity {
        machine_identity: host.machine_identity,
        host_identity: host.host_identity,
        boot_identity: host.boot_identity,
        pid: std::process::id(),
        process_start_identity: process_start_identity(std::process::id())
            .unwrap_or_else(|| format!("process-start-unavailable-{}", std::process::id())),
        claim_id: Uuid::new_v4().to_string(),
    };
    if register_claim {
        process_claim_registry()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(owner.claim_id.clone());
    }
    owner
}

pub(crate) fn current_process_owner() -> Result<ProcessOwnerIdentity, ActionError> {
    let owner = process_owner_identity(true);
    let machine_known = identity_is_available(&owner.machine_identity, "machine-id-unavailable");
    let boot_known = identity_is_available(&owner.boot_identity, "boot-id-unavailable");
    if !machine_known || !boot_known || !process_start_identity_is_strong(&owner.process_start_identity) {
        release_current_process_owner_claim(&owner.claim_id);
        return Err(ActionError::Election(
            "cannot create durable workspace ownership without strong machine, boot-session, and process-start identity"
                .to_string(),
        ));
    }
    Ok(owner)
}

pub(crate) fn validate_owner_identity(owner: &ProcessOwnerIdentity) -> Result<(), ActionError> {
    if owner.machine_identity.is_empty()
        || owner.host_identity.is_empty()
        || owner.boot_identity.is_empty()
        || owner.process_start_identity.is_empty()
        || owner.claim_id.is_empty()
        || owner.pid == 0
    {
        return Err(ActionError::Election(
            "claim owner identity is incomplete".to_string(),
        ));
    }
    if Uuid::parse_str(&owner.claim_id).is_err() {
        return Err(ActionError::Election(
            "claim owner identity contains an invalid claim UUID".to_string(),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OwnerLiveness {
    Alive,
    DeadLocal,
    RemoteOrUnknown,
}

pub(crate) fn owner_liveness(
    owner: &ProcessOwnerIdentity,
) -> Result<OwnerLiveness, ActionError> {
    let current = process_owner_identity(false);

    if owner.pid == current.pid
        && owner.process_start_identity == current.process_start_identity
        && owner.machine_identity == current.machine_identity
        && owner.host_identity == current.host_identity
        && owner.boot_identity == current.boot_identity
    {
        if process_claim_is_registered(&owner.claim_id) {
            return Ok(OwnerLiveness::Alive);
        }
        // The identity tuple proves this is OUR OWN process instance, so an
        // unregistered claim is an abandoned earlier attempt (released or
        // failed), not another process: a same-process retry must be able to
        // retire it. With a weak start identity the tuple cannot prove
        // instance identity, so fail closed.
        if process_start_identity_is_strong(&owner.process_start_identity) {
            return Ok(OwnerLiveness::DeadLocal);
        }
        return Ok(OwnerLiveness::Alive);
    }

    let machine_known = identity_is_available(&owner.machine_identity, "machine-id-unavailable")
        && identity_is_available(&current.machine_identity, "machine-id-unavailable");
    let host_known = identity_is_available(&owner.host_identity, "host-unavailable")
        && identity_is_available(&current.host_identity, "host-unavailable");
    let boot_known = identity_is_available(&owner.boot_identity, "boot-id-unavailable")
        && identity_is_available(&current.boot_identity, "boot-id-unavailable");

    if machine_known && owner.machine_identity != current.machine_identity {
        return Ok(OwnerLiveness::RemoteOrUnknown);
    }

    // Hostname is supplemental rather than sufficient identity. A rename
    // during the same boot must not wedge recovery, but a different hostname
    // combined with a different boot ID protects against cloned machine IDs on
    // a shared filesystem.
    let hostname_differs = host_known && owner.host_identity != current.host_identity;

    // Without strong local-machine and boot evidence, never infer staleness
    // from a local PID lookup. The claim may live on a shared filesystem and
    // belong to a different host with the same PID.
    if !machine_known || !boot_known {
        return Ok(OwnerLiveness::RemoteOrUnknown);
    }
    if owner.boot_identity != current.boot_identity {
        return Ok(if hostname_differs {
            OwnerLiveness::RemoteOrUnknown
        } else {
            OwnerLiveness::DeadLocal
        });
    }
    if !process_start_identity_is_strong(&owner.process_start_identity)
        || !process_start_identity_is_strong(&current.process_start_identity)
    {
        return Ok(OwnerLiveness::RemoteOrUnknown);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        return match local_process_start_identity(owner.pid) {
            Ok(Some(start)) if start == owner.process_start_identity => Ok(OwnerLiveness::Alive),
            Ok(Some(_)) | Ok(None) => Ok(OwnerLiveness::DeadLocal),
            Err(_) => Ok(OwnerLiveness::RemoteOrUnknown),
        };
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Ok(OwnerLiveness::RemoteOrUnknown)
    }
}

/// True when the owner tuple provably names THIS process instance but its
/// claim is no longer registered: an abandoned earlier attempt of the same
/// live process (failed or superseded), which same-process retry may retire.
pub(crate) fn owner_is_abandoned_current_process_attempt(
    owner: &ProcessOwnerIdentity,
) -> bool {
    let current = process_owner_identity(false);
    owner.pid == current.pid
        && owner.process_start_identity == current.process_start_identity
        && owner.machine_identity == current.machine_identity
        && owner.host_identity == current.host_identity
        && owner.boot_identity == current.boot_identity
        && process_start_identity_is_strong(&owner.process_start_identity)
        && !process_claim_is_registered(&owner.claim_id)
}

pub(crate) fn owner_is_current_process(
    owner: &ProcessOwnerIdentity,
) -> Result<bool, ActionError> {
    validate_owner_identity(owner)?;
    let current = process_owner_identity(false);
    Ok(owner.pid == current.pid
        && owner.process_start_identity == current.process_start_identity
        && owner.machine_identity == current.machine_identity
        && owner.host_identity == current.host_identity
        && owner.boot_identity == current.boot_identity
        // Current-process mutation authority requires the exact in-memory
        // claim UUID, not merely a matching PID/start tuple. The tuple proves
        // liveness; the registered claim proves this process instance created
        // and still owns this specific durable workspace authority.
        && process_claim_is_registered(&owner.claim_id))
}

fn process_start_identity_is_strong(value: &str) -> bool {
    !value.trim().is_empty() && !value.starts_with("process-start-unavailable-")
}

fn identity_is_available(value: &str, unavailable: &str) -> bool {
    !value.trim().is_empty() && value != unavailable && !value.starts_with("process-start-unavailable-")
}

fn process_start_identity(pid: u32) -> Option<String> {
    local_process_start_identity(pid).ok().flatten()
}

// ---------------------------------------------------------------------------
// Path, durability, and identity helpers
// ---------------------------------------------------------------------------

fn action_journal_path(
    context: &ActionContext,
    pipeline_sha256: &str,
) -> Result<PathBuf, ActionError> {
    validate_mutation_path(&context.journal_dir, false)?;
    let mut hasher = Sha256::new();
    hasher.update(context.run_identity.as_bytes());
    hasher.update([0]);
    hasher.update(context.album_identity.as_bytes());
    hasher.update([0]);
    hasher.update(context.phase.as_str().as_bytes());
    hasher.update([0]);
    hasher.update(pipeline_sha256.as_bytes());
    Ok(context.journal_dir.join(format!(
        "actions-{}-{}.journal.json",
        context.phase.as_str(),
        hex::encode(hasher.finalize())
    )))
}

fn operation_id(claim_id: &str, action_index: usize, operation_index: usize) -> String {
    format!("{claim_id}:{action_index:06}:{operation_index:06}")
}

fn script_runtime_directory(
    context: &ActionContext,
    claim_id: &str,
    action_index: usize,
    operation_index: usize,
) -> Result<PathBuf, ActionError> {
    validate_mutation_path(&context.journal_dir, false)?;
    Ok(context.journal_dir.join(format!(
        "{INTERNAL_WORKSPACE_PREFIX}{claim_id}-script-{action_index:06}-{operation_index:06}"
    )))
}

fn script_containment_token(
    claim_id: &str,
    action_index: usize,
    operation_index: usize,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(claim_id.as_bytes());
    hasher.update([0]);
    hasher.update(action_index.to_be_bytes());
    hasher.update(operation_index.to_be_bytes());
    hasher.update(b"runscript-containment-v1");
    hex::encode(hasher.finalize())[..32].to_string()
}

fn rename_staging_root(
    context: &ActionContext,
    claim_id: &str,
    action_index: usize,
) -> Result<PathBuf, ActionError> {
    let parent = context.subject_dir.parent().ok_or_else(|| {
        ActionError::UnsafePath(format!(
            "subject directory has no parent: {}",
            context.subject_dir.display()
        ))
    })?;
    Ok(parent.join(format!(
        "{INTERNAL_WORKSPACE_PREFIX}{claim_id}-rename-{action_index:06}"
    )))
}

fn action_temporary_path(
    destination: &Path,
    claim_id: &str,
    action_index: usize,
    operation_index: usize,
    purpose: &str,
) -> Result<PathBuf, ActionError> {
    let parent = destination.parent().ok_or_else(|| {
        ActionError::UnsafePath(format!(
            "destination has no parent: {}",
            destination.display()
        ))
    })?;
    Ok(parent.join(format!(
        "{INTERNAL_WORKSPACE_PREFIX}{claim_id}-{purpose}-{action_index:06}-{operation_index:06}"
    )))
}

fn publication_witness_path(
    destination: &Path,
    claim_id: &str,
    action_index: usize,
    operation_index: usize,
    kind: ObjectKind,
) -> Result<PathBuf, ActionError> {
    match kind {
        ObjectKind::Directory => Ok(destination.join(format!(
            "{INTERNAL_WORKSPACE_PREFIX}{claim_id}-publish-witness-{action_index:06}-{operation_index:06}.json"
        ))),
        ObjectKind::File => action_temporary_path(
            destination,
            claim_id,
            action_index,
            operation_index,
            "publish-witness",
        ),
    }
}

fn operation_expected_kind(operation: &PlannedOperation) -> Result<ObjectKind, ActionError> {
    match operation {
        PlannedOperation::Copy { expected_source, .. }
        | PlannedOperation::Move { expected_source, .. } => Ok(expected_source.kind),
        _ => Err(ActionError::InvalidJournal(
            "publication witness requested for a non-copy/move operation".to_string(),
        )),
    }
}

fn validate_publication_witness_path(
    witness: &Path,
    destination: &Path,
    claim_id: &str,
    kind: ObjectKind,
) -> Result<(), ActionError> {
    match kind {
        ObjectKind::File => validate_owned_temporary(witness, destination.parent(), claim_id),
        ObjectKind::Directory => {
            if witness.parent() != Some(destination)
                || !is_action_owned_path(witness, claim_id)
                || !witness
                    .file_name()
                    .and_then(|value| value.to_str())
                    .map(|name| name.ends_with(".json"))
                    .unwrap_or(false)
            {
                return Err(ActionError::InvalidJournal(format!(
                    "directory publication witness is foreign: {}",
                    witness.display()
                )));
            }
            Ok(())
        }
    }
}

fn prepublication_witness_path(
    publication_witness: &Path,
    temporary: &Path,
    expected: &ObjectIdentity,
) -> Result<PathBuf, ActionError> {
    if expected.kind == ObjectKind::File {
        return Ok(publication_witness.to_path_buf());
    }
    let file_name = publication_witness.file_name().ok_or_else(|| {
        ActionError::InvalidJournal("directory publication witness has no file name".to_string())
    })?;
    Ok(temporary.join(file_name))
}

fn same_directory_witness(
    source: &Path,
    claim_id: &str,
    action_index: usize,
    operation_index: usize,
    purpose: &str,
) -> Result<PathBuf, ActionError> {
    let parent = source.parent().ok_or_else(|| {
        ActionError::UnsafePath(format!("source has no parent: {}", source.display()))
    })?;
    Ok(parent.join(format!(
        "{INTERNAL_WORKSPACE_PREFIX}{claim_id}-{purpose}-{action_index:06}-{operation_index:06}"
    )))
}

fn is_action_owned_path(path: &Path, claim_id: &str) -> bool {
    path.components().any(|component| {
        component.as_os_str().to_string_lossy().starts_with(&format!(
            "{INTERNAL_WORKSPACE_PREFIX}{claim_id}"
        ))
    })
}

fn render_action_path(
    configured: &Path,
    context: &ActionContext,
    relative_base: &Path,
) -> Result<PathBuf, ActionError> {
    let text = configured.to_string_lossy();
    let rendered = (context.semantics.render_template)(&text, &context.album_tokens)
        .map_err(ActionError::Conflict)?;
    let expanded = expand_home(&rendered)?;
    let path = if expanded.is_absolute() {
        expanded
    } else {
        relative_base.join(expanded)
    };
    lexical_normalize_absolute(&path)
}

fn resolve_script_path(configured: &Path) -> Result<PathBuf, ActionError> {
    let text = configured.to_string_lossy();
    if text.trim().is_empty() {
        return Err(ActionError::Script("script path is empty".to_string()));
    }
    let expanded = expand_home(&text)?;
    let resolved = if expanded.is_absolute() {
        expanded
    } else {
        let home = dirs::home_dir().ok_or_else(|| {
            ActionError::Script("cannot resolve relative script path without a home directory".to_string())
        })?;
        home.join(".config")
            .join("tonepoet")
            .join("scripts")
            .join(expanded)
    };
    lexical_normalize_absolute(&resolved)
}

fn expand_home(text: &str) -> Result<PathBuf, ActionError> {
    if text == "~" || text.starts_with("~/") || text.starts_with("~\\") {
        let home = dirs::home_dir().ok_or_else(|| {
            ActionError::UnsafePath("cannot expand ~ without a home directory".to_string())
        })?;
        if text.len() == 1 {
            return Ok(home);
        }
        return Ok(home.join(&text[2..]));
    }
    if text.starts_with('~') {
        return Err(ActionError::UnsafePath(
            "~user expansion is not supported; use an absolute path".to_string(),
        ));
    }
    Ok(PathBuf::from(text))
}

fn validate_executable_script(path: &Path) -> Result<(), ActionError> {
    validate_mutation_path(path, false)?;
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        ActionError::Script(format!("script {} is unavailable: {error}", path.display()))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(ActionError::Script(format!(
            "script must be a regular non-symlink file: {}",
            path.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(ActionError::Script(format!(
                "script is not executable: {}",
                path.display()
            )));
        }
    }
    Ok(())
}

fn validate_context_syntax(context: &ActionContext) -> Result<(), ActionError> {
    if context.run_identity.trim().is_empty() || context.album_identity.trim().is_empty() {
        return Err(ActionError::Conflict(
            "run and album identities must be non-empty".to_string(),
        ));
    }
    validate_mutation_path(&context.subject_dir, false)?;
    validate_mutation_path(&context.source_path, false)?;
    validate_mutation_path(&context.output_root, false)?;
    validate_mutation_path(&context.album_dir, false)?;
    validate_mutation_path(&context.journal_dir, false)?;
    if !context.explicit_scope {
        let output_root = lexical_normalize_absolute(&context.output_root)?;
        let album_dir = lexical_normalize_absolute(&context.album_dir)?;
        if album_dir == output_root || !album_dir.starts_with(&output_root) {
            return Err(ActionError::Conflict(format!(
                "{} actions skipped: rendered album directory must be a proper subdirectory of output root ({} vs {})",
                context.phase.as_str(),
                album_dir.display(),
                output_root.display()
            )));
        }
        if context.phase == ActionPhase::Post
            && lexical_normalize_absolute(&context.subject_dir)? != album_dir
        {
            return Err(ActionError::Conflict(
                "post action subject differs from the published album directory".to_string(),
            ));
        }
    }
    if context.explicit_scope && is_filesystem_root(&context.subject_dir) {
        return Err(ActionError::UnsafePath(format!(
            ":actions-run refuses a filesystem root: {}",
            context.subject_dir.display()
        )));
    }
    Ok(())
}

fn validate_context(context: &ActionContext) -> Result<(), ActionError> {
    validate_context_syntax(context)?;
    if !context.subject_dir.is_dir() {
        return Err(ActionError::Conflict(format!(
            "action subject is not a directory: {}",
            context.subject_dir.display()
        )));
    }
    Ok(())
}

fn context_has_retained_capabilities(context: &ActionContext) -> bool {
    context.retained_album_capability.is_some()
        || context.retained_output_capability.is_some()
        || context.retained_journal_capability.is_some()
}

fn validate_context_through_capabilities(
    filesystem: &dyn ActionFilesystem,
    context: &ActionContext,
) -> Result<(), ActionError> {
    let subject = filesystem.entry_identity(&context.subject_dir)?.ok_or_else(|| {
        ActionError::Conflict(format!(
            "action subject is not a directory through its installed capability: {}",
            context.subject_dir.display()
        ))
    })?;
    if subject.file_type != CapFileType::Directory {
        return Err(ActionError::Conflict(format!(
            "action subject is not a directory through its installed capability: {}",
            context.subject_dir.display()
        )));
    }

    // A retained descriptor is the live object authority.  Prove that each
    // stable logical root resolves through the installed capability registry
    // to that exact directory identity; never re-resolve the lexical pathname.
    for (label, logical_path, retained) in [
        (
            "album",
            context.album_dir.as_path(),
            context.retained_album_capability.as_deref(),
        ),
        (
            "output",
            context.output_root.as_path(),
            context.retained_output_capability.as_deref(),
        ),
        (
            "journal",
            context.journal_dir.as_path(),
            context.retained_journal_capability.as_deref(),
        ),
    ] {
        let Some(retained) = retained else {
            continue;
        };
        let observed = filesystem.entry_identity(logical_path)?.ok_or_else(|| {
            ActionError::Conflict(format!(
                "retained {label} capability is not installed for logical root {}",
                logical_path.display()
            ))
        })?;
        if observed.file_type != CapFileType::Directory || observed != retained.identity() {
            return Err(ActionError::Conflict(format!(
                "retained {label} capability no longer identifies logical root {}",
                logical_path.display()
            )));
        }
    }
    Ok(())
}

fn prepare_and_validate_context_capabilities(
    filesystem: &dyn ActionFilesystem,
    context: &ActionContext,
) -> Result<(), ActionError> {
    validate_context_syntax(context)?;
    prepare_context_capabilities(filesystem, context)?;
    validate_context_through_capabilities(filesystem, context)
}

fn prepare_context_for_journal_read(
    filesystem: &dyn ActionFilesystem,
    context: &ActionContext,
) -> Result<bool, ActionError> {
    validate_context_syntax(context)?;
    if !context_has_retained_capabilities(context) {
        validate_context(context)?;
        return Ok(false);
    }
    if context.retained_journal_capability.is_none() {
        return Err(ActionError::Contradiction(
            "live capability-bound action context is missing its retained journal directory"
                .to_string(),
        ));
    }
    prepare_context_capabilities(filesystem, context)?;
    validate_context_through_capabilities(filesystem, context)?;
    Ok(true)
}

fn validate_mutation_path(path: &Path, allow_root: bool) -> Result<(), ActionError> {
    if path.as_os_str().is_empty() || !path.is_absolute() {
        return Err(ActionError::UnsafePath(format!(
            "path must be non-empty and absolute: {}",
            path.display()
        )));
    }
    if !allow_root && is_filesystem_root(path) {
        return Err(ActionError::UnsafePath(format!(
            "filesystem root is not an action target: {}",
            path.display()
        )));
    }
    for component in path.components() {
        if matches!(component, Component::CurDir | Component::ParentDir) {
            return Err(ActionError::UnsafePath(format!(
                "path contains an unstable dot component: {}",
                path.display()
            )));
        }
    }
    Ok(())
}

/// Atomically rename without replacing an existing destination. Directory
/// copy publication and rename staging require the kernel's no-replace
/// semantic; an existence check followed by `rename` is not sufficient under
/// concurrent mutation.
#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_vendor = "apple",
    target_os = "redox"
))]
fn rename_path_no_clobber(source: &Path, destination: &Path) -> Result<(), ActionError> {
    use rustix::fs::{renameat_with, RenameFlags};

    let source_parent = source.parent().ok_or_else(|| {
        ActionError::UnsafePath(format!("rename source has no parent: {}", source.display()))
    })?;
    let destination_parent = destination.parent().ok_or_else(|| {
        ActionError::UnsafePath(format!(
            "rename destination has no parent: {}",
            destination.display()
        ))
    })?;
    let source_name = source.file_name().ok_or_else(|| {
        ActionError::UnsafePath(format!("rename source has no file name: {}", source.display()))
    })?;
    let destination_name = destination.file_name().ok_or_else(|| {
        ActionError::UnsafePath(format!(
            "rename destination has no file name: {}",
            destination.display()
        ))
    })?;
    let source_directory = File::open(source_parent)?;
    let destination_directory = File::open(destination_parent)?;
    match renameat_with(
        &source_directory,
        source_name,
        &destination_directory,
        destination_name,
        RenameFlags::NOREPLACE,
    ) {
        Ok(()) => Ok(()),
        Err(error) if error == rustix::io::Errno::EXIST => Err(ActionError::Conflict(format!(
            "destination already exists: {}",
            destination.display()
        ))),
        Err(error) => Err(ActionError::Io(error.into())),
    }
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_vendor = "apple",
    target_os = "redox"
)))]
fn rename_path_no_clobber(_source: &Path, destination: &Path) -> Result<(), ActionError> {
    Err(ActionError::Io(io::Error::new(
        io::ErrorKind::Unsupported,
        format!(
            "atomic no-clobber directory rename is unavailable on this platform: {}",
            destination.display()
        ),
    )))
}

fn validate_target_under_subject(path: &Path, subject: &Path) -> Result<(), ActionError> {
    let path = lexical_normalize_absolute(path)?;
    let subject = lexical_normalize_absolute(subject)?;
    if path == subject || !path.starts_with(&subject) {
        return Err(ActionError::UnsafePath(format!(
            "target is outside action subject {}: {}",
            subject.display(),
            path.display()
        )));
    }
    Ok(())
}

fn checked_relative_target(pattern: &str) -> Result<PathBuf, ActionError> {
    let path = Path::new(pattern);
    if path.is_absolute() || path.as_os_str().is_empty() {
        return Err(ActionError::UnsafePath(format!(
            "exact target must be a non-empty relative path: {pattern}"
        )));
    }
    for component in path.components() {
        if !matches!(component, Component::Normal(_)) {
            return Err(ActionError::UnsafePath(format!(
                "exact target contains dot/root components: {pattern}"
            )));
        }
    }
    Ok(path.to_path_buf())
}

fn validate_single_component(component: &str) -> Result<(), ActionError> {
    if component.trim().is_empty() {
        return Err(ActionError::Conflict(
            "rename rendered an empty component".to_string(),
        ));
    }
    let path = Path::new(component);
    if path.components().count() != 1
        || !matches!(path.components().next(), Some(Component::Normal(_)))
    {
        return Err(ActionError::UnsafePath(format!(
            "rename rendered a non-component path: {component}"
        )));
    }
    Ok(())
}

fn lexical_normalize_absolute(path: &Path) -> Result<PathBuf, ActionError> {
    if !path.is_absolute() {
        return Err(ActionError::UnsafePath(format!(
            "path is not absolute: {}",
            path.display()
        )));
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err(ActionError::UnsafePath(format!(
                        "path escapes filesystem root: {}",
                        path.display()
                    )));
                }
            }
            Component::Normal(value) => normalized.push(value),
        }
    }
    Ok(normalized)
}

/// Infallible lexical normalization for comparison purposes. Absolute paths
/// normalize like `lexical_normalize_absolute`; anything that cannot be
/// normalized safely compares as itself (conservative for exclusion checks).
fn lexical_normalize(path: &Path) -> PathBuf {
    lexical_normalize_absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

fn now_unix_nanos() -> u64 {
    // u64 nanoseconds cover the epoch through the year 2554; serialized
    // 128-bit integers cannot pass through serde's internally-tagged enum
    // buffering ("i128 is not supported"), so journal stamps stay 64-bit.
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Env hygiene (brief: strip control characters and newlines before export —
/// tag values are semi-trusted input).
fn sanitize_env_value(value: &str) -> String {
    value.chars().filter(|ch| !ch.is_control()).collect()
}

/// POSIX-shaped environment key: alphanumeric/underscore, not digit-leading.
fn valid_environment_key(key: &str) -> bool {
    let mut chars = key.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    key.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

fn is_filesystem_root(path: &Path) -> bool {
    path.parent().is_none()
}

fn contains_wildcard(pattern: &str) -> bool {
    pattern.contains('*') || pattern.contains('?')
}

fn is_hidden_name(name: &OsStr) -> bool {
    name.to_string_lossy().starts_with('.')
}

fn paths_refer_to_same_location(left: &Path, right: &Path) -> bool {
    if lexical_normalize_absolute(left).ok() == lexical_normalize_absolute(right).ok() {
        return true;
    }
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

/// Number of leading components that form a trusted descriptor-namespace
/// anchor (`/proc/self/fd/N` on Linux, `/dev/fd/N` elsewhere), counting the
/// root component. Zero when the path is an ordinary logical path.
fn descriptor_namespace_anchor_components(path: &Path) -> usize {
    let mut parts = path.components();
    if !matches!(parts.next(), Some(Component::RootDir)) {
        return 0;
    }
    let as_str = |component: Option<Component<'_>>| -> Option<String> {
        match component {
            Some(Component::Normal(value)) => Some(value.to_string_lossy().into_owned()),
            _ => None,
        }
    };
    let first = as_str(parts.next());
    let second = as_str(parts.next());
    let third = as_str(parts.next());
    let fourth = as_str(parts.next());
    match (first.as_deref(), second.as_deref(), third.as_deref(), fourth.as_deref()) {
        (Some("proc"), Some("self"), Some("fd"), Some(n))
            if n.bytes().all(|b| b.is_ascii_digit()) =>
        {
            5
        }
        (Some("dev"), Some("fd"), Some(n), _) if n.bytes().all(|b| b.is_ascii_digit()) => 4,
        _ => 0,
    }
}

fn create_dir_all_no_symlink(path: &Path) -> Result<(), ActionError> {
    validate_mutation_path(path, true)?;
    // A retained-descriptor route is a trusted anchor: /proc/self and the fd
    // link ARE symlinks by construction, and their trust was established when
    // the descriptor was pinned. No-symlink validation applies strictly to
    // every component BELOW the anchor.
    let anchor_components = descriptor_namespace_anchor_components(path);
    let mut current = PathBuf::new();
    for (index, component) in path.components().enumerate() {
        current.push(component.as_os_str());
        if index < anchor_components {
            continue;
        }
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(ActionError::UnsafePath(format!(
                    "directory path traverses a symlink: {}",
                    current.display()
                )));
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(ActionError::UnsafePath(format!(
                    "directory path traverses a non-directory: {}",
                    current.display()
                )));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                fs::create_dir(&current)?;
                sync_parent(&current)?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn capability_object_identity(
    filesystem: &CapabilityFilesystem,
    path: &ScopedPath,
    include_hidden: bool,
    excluded_descendants: &[ScopedPath],
) -> Result<ObjectIdentity, ActionError> {
    let metadata = filesystem.metadata_no_follow(path)?.ok_or_else(|| {
        ActionError::Io(io::Error::new(
            io::ErrorKind::NotFound,
            format!("capability operand vanished: {}", filesystem.display_path(path).map(|p| p.display().to_string()).unwrap_or_default()),
        ))
    })?;
    if matches!(metadata.file_type, CapFileType::Symlink | CapFileType::Other) {
        return Err(ActionError::UnsafePath(format!(
            "capability operand is a symlink or special file: {}",
            filesystem.display_path(path)?.display()
        )));
    }
    let root_identity = filesystem_identity_from_cap(metadata);
    if metadata.file_type == CapFileType::Regular {
        let mut file = filesystem.open_regular_read_checked(path, metadata)?;
        let mut hasher = Sha256::new();
        let mut buffer = [0_u8; 128 * 1024];
        let mut byte_length = 0_u64;
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hasher.update(&buffer[..count]);
            byte_length = byte_length.saturating_add(count as u64);
        }
        return Ok(ObjectIdentity {
            kind: ObjectKind::File,
            content_sha256: hex::encode(hasher.finalize()),
            byte_length,
            entry_count: 1,
            copy_metadata: CopyMetadataIdentity {
                root: copy_metadata_entry(PathBuf::new(), ObjectKind::File, metadata),
                descendants: Vec::new(),
            },
            filesystem: root_identity,
        });
    }

    let excluded = |candidate: &ScopedPath| {
        excluded_descendants.iter().any(|entry| {
            entry.scope == candidate.scope
                && (entry.relative == candidate.relative
                    || candidate.relative.as_path().starts_with(entry.relative.as_path()))
        })
    };
    let mut entries = Vec::<(PathBuf, ScopedPath, bool, CapMetadata)>::new();
    let mut stack = vec![(path.clone(), metadata)];
    while let Some((directory, expected_directory)) = stack.pop() {
        let mut children = filesystem.enumerate_checked(&directory, expected_directory)?;
        children.sort_by(|left, right| left.name.cmp(&right.name));
        for child in children {
            if !include_hidden && is_hidden_name(&child.name) {
                continue;
            }
            let child_path = ScopedPath {
                scope: directory.scope.clone(),
                relative: directory.relative.join(&child.name)?,
            };
            if excluded(&child_path) {
                continue;
            }
            let relative = child_path
                .relative
                .as_path()
                .strip_prefix(path.relative.as_path())
                .map_err(|_| ActionError::UnsafePath("capability identity escaped root".to_string()))?
                .to_path_buf();
            match child.metadata.file_type {
                CapFileType::Directory => {
                    entries.push((relative, child_path.clone(), true, child.metadata));
                    stack.push((child_path, child.metadata));
                }
                CapFileType::Regular => {
                    entries.push((relative, child_path, false, child.metadata))
                }
                CapFileType::Symlink | CapFileType::Other => {
                    return Err(ActionError::UnsafePath(format!(
                        "capability identity encountered a symlink or special file: {}",
                        filesystem.display_path(&child_path)?.display()
                    )))
                }
            }
        }
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    let copy_metadata_descendants = entries
        .iter()
        .map(|(relative, _, is_directory, metadata)| {
            copy_metadata_entry(
                relative.clone(),
                if *is_directory {
                    ObjectKind::Directory
                } else {
                    ObjectKind::File
                },
                *metadata,
            )
        })
        .collect();
    let mut hasher = Sha256::new();
    let mut byte_length = 0_u64;
    let mut entry_count = 0_u64;
    for (relative, child, is_directory, expected_child) in entries {
        entry_count = entry_count.saturating_add(1);
        hasher.update(if is_directory { b"D\0" } else { b"F\0" });
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            hasher.update(relative.as_os_str().as_bytes());
        }
        #[cfg(not(unix))]
        hasher.update(relative.to_string_lossy().as_bytes());
        hasher.update([0]);
        if !is_directory {
            let mut file = filesystem.open_regular_read_checked(&child, expected_child)?;
            let mut buffer = [0_u8; 128 * 1024];
            loop {
                let count = file.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                hasher.update(&buffer[..count]);
                byte_length = byte_length.saturating_add(count as u64);
            }
        }
    }
    Ok(ObjectIdentity {
        kind: ObjectKind::Directory,
        content_sha256: hex::encode(hasher.finalize()),
        byte_length,
        entry_count,
        copy_metadata: CopyMetadataIdentity {
            root: copy_metadata_entry(PathBuf::new(), ObjectKind::Directory, metadata),
            descendants: copy_metadata_descendants,
        },
        filesystem: root_identity,
    })
}

fn copy_metadata_entry(
    relative_path: PathBuf,
    kind: ObjectKind,
    metadata: CapMetadata,
) -> CopyMetadataEntry {
    CopyMetadataEntry {
        relative_path,
        kind,
        mode: metadata.mode & 0o7777,
        modified_nanos: metadata
            .modified_seconds
            .saturating_mul(1_000_000_000)
            .saturating_add(metadata.modified_nanos),
    }
}

fn cap_entry_identity(identity: &ObjectIdentity) -> Result<CapEntryIdentity, ActionError> {
    let file_type = match identity.kind {
        ObjectKind::File => CapFileType::Regular,
        ObjectKind::Directory => CapFileType::Directory,
    };
    let device = identity.filesystem.device.ok_or_else(|| {
        ActionError::InvalidJournal("planned object identity is missing a device id".to_string())
    })?;
    let inode = identity.filesystem.inode.ok_or_else(|| {
        ActionError::InvalidJournal("planned object identity is missing an inode id".to_string())
    })?;
    Ok(CapEntryIdentity {
        file_type,
        device,
        inode,
    })
}

fn filesystem_identity_from_cap(metadata: CapMetadata) -> FilesystemIdentity {
    FilesystemIdentity {
        device: Some(metadata.device),
        inode: Some(metadata.inode),
        length: metadata.length,
        modified_nanos: Some(
            (metadata.modified_seconds.max(0) as u64)
                .saturating_mul(1_000_000_000)
                .saturating_add(metadata.modified_nanos.max(0) as u64),
        ),
        changed_nanos: Some(
            metadata
                .changed_seconds
                .saturating_mul(1_000_000_000)
                .saturating_add(metadata.changed_nanos),
        ),
    }
}

#[cfg(test)]
mod conversion_actions_tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn durable_action_identity_rejects_descriptor_namespace_paths() {
        assert!(reject_ephemeral_descriptor_namespace_path(
            "album",
            Path::new("/proc/self/fd/17/Album"),
        )
        .is_err());
        assert!(reject_ephemeral_descriptor_namespace_path(
            "album",
            Path::new("/dev/fd/9/Album"),
        )
        .is_err());
        assert!(reject_ephemeral_descriptor_namespace_path(
            "album",
            Path::new("/srv/music/Album"),
        )
        .is_ok());
    }

    #[derive(Default)]
    struct RecordingRunner {
        calls: Arc<Mutex<Vec<ScriptInvocation>>>,
    }

    #[cfg(unix)]
    struct PublicationLockProbeRunner {
        album_dir: PathBuf,
        probed: Arc<Mutex<bool>>,
    }

    struct ExecGatedCancellationRunner;

    struct PanicAfterPreparedRunner;

    #[derive(Default)]
    struct RecoveryOnlyRunner {
        run_calls: Arc<Mutex<usize>>,
        recover_calls: Arc<Mutex<usize>>,
    }

    #[derive(Default)]
    struct NeverReleasedRecoveryRunner {
        run_calls: Arc<Mutex<usize>>,
        recover_calls: Arc<Mutex<usize>>,
    }

    #[derive(Default)]
    struct FailAfterPreparedRunner {
        recover_calls: Arc<Mutex<usize>>,
    }

    impl ActionScriptRunner for PanicAfterPreparedRunner {
        fn run(
            &self,
            invocation: &ScriptInvocation,
            _cancellation: &dyn ActionCancellation,
            observer: &mut dyn FnMut(&ScriptLifecycleEvent) -> Result<(), ActionError>,
        ) -> Result<ScriptOutcome, ActionError> {
            let _ = emit_prepared(invocation, observer)?;
            panic!("injected application crash after durable script start");
        }

        fn recover(
            &self,
            _request: &ScriptRecoveryRequest,
            _observer: &mut dyn FnMut(&ScriptLifecycleEvent) -> Result<(), ActionError>,
        ) -> Result<ScriptRecoveryOutcome, ActionError> {
            panic!("crash-injection runner must not recover")
        }

        fn cleanup(&self, _request: &ScriptRecoveryRequest) -> Result<(), ActionError> {
            Ok(())
        }
    }


    impl ActionScriptRunner for FailAfterPreparedRunner {
        fn run(
            &self,
            invocation: &ScriptInvocation,
            _cancellation: &dyn ActionCancellation,
            observer: &mut dyn FnMut(&ScriptLifecycleEvent) -> Result<(), ActionError>,
        ) -> Result<ScriptOutcome, ActionError> {
            let _ = emit_prepared(invocation, observer)?;
            Err(ActionError::Script(
                "injected live supervisor failure after durable preparation".to_string(),
            ))
        }

        fn recover(
            &self,
            request: &ScriptRecoveryRequest,
            observer: &mut dyn FnMut(&ScriptLifecycleEvent) -> Result<(), ActionError>,
        ) -> Result<ScriptRecoveryOutcome, ActionError> {
            *self.recover_calls.lock().unwrap() += 1;
            observer(&ScriptLifecycleEvent::TerminationRequested {
                schema_version: 1,
                reason: TerminationReason::Recovery,
                graceful_deadline_unix_millis: 1,
            })?;
            observer(&ScriptLifecycleEvent::ContainmentEmpty {
                schema_version: 1,
                confidence: request.descriptor.confidence,
            })?;
            Ok(ScriptRecoveryOutcome::ContainmentTerminated)
        }

        fn cleanup(&self, _request: &ScriptRecoveryRequest) -> Result<(), ActionError> {
            Ok(())
        }
    }

    impl ActionScriptRunner for RecoveryOnlyRunner {
        fn run(
            &self,
            _invocation: &ScriptInvocation,
            _cancellation: &dyn ActionCancellation,
            _observer: &mut dyn FnMut(&ScriptLifecycleEvent) -> Result<(), ActionError>,
        ) -> Result<ScriptOutcome, ActionError> {
            *self.run_calls.lock().unwrap() += 1;
            Err(ActionError::Script(
                "durably started script was incorrectly replayed".to_string(),
            ))
        }

        fn recover(
            &self,
            _request: &ScriptRecoveryRequest,
            _observer: &mut dyn FnMut(&ScriptLifecycleEvent) -> Result<(), ActionError>,
        ) -> Result<ScriptRecoveryOutcome, ActionError> {
            *self.recover_calls.lock().unwrap() += 1;
            Ok(ScriptRecoveryOutcome::ContainmentAlreadyEmpty)
        }

        fn cleanup(&self, _request: &ScriptRecoveryRequest) -> Result<(), ActionError> {
            Ok(())
        }
    }

    impl ActionScriptRunner for NeverReleasedRecoveryRunner {
        fn run(
            &self,
            _invocation: &ScriptInvocation,
            _cancellation: &dyn ActionCancellation,
            _observer: &mut dyn FnMut(&ScriptLifecycleEvent) -> Result<(), ActionError>,
        ) -> Result<ScriptOutcome, ActionError> {
            *self.run_calls.lock().unwrap() += 1;
            Err(ActionError::Script(
                "exec-gated script was incorrectly replayed".to_string(),
            ))
        }

        fn recover(
            &self,
            request: &ScriptRecoveryRequest,
            observer: &mut dyn FnMut(&ScriptLifecycleEvent) -> Result<(), ActionError>,
        ) -> Result<ScriptRecoveryOutcome, ActionError> {
            *self.recover_calls.lock().unwrap() += 1;
            observer(&ScriptLifecycleEvent::ContainmentEmpty {
                schema_version: 1,
                confidence: request.descriptor.confidence,
            })?;
            Ok(ScriptRecoveryOutcome::ExecutionNeverReleased)
        }

        fn cleanup(&self, _request: &ScriptRecoveryRequest) -> Result<(), ActionError> {
            Ok(())
        }
    }

    fn test_descriptor(invocation: &ScriptInvocation) -> ContainmentDescriptor {
        ContainmentDescriptor {
            schema_version: 1,
            token: invocation.containment_token.clone(),
            backend: crate::convert::script_supervisor::ContainmentBackend::LinuxSubreaper,
            confidence: ContainmentConfidence::ProcessTreeObserved,
            host: HostBootIdentity {
                machine_identity: "test-machine".to_string(),
                host_identity: "test-host".to_string(),
                boot_identity: "test-boot".to_string(),
            },
            supervisor: StableProcessIdentity {
                pid: 100,
                start_identity: "1000".to_string(),
            },
            leader: StableProcessIdentity {
                pid: 101,
                start_identity: "1001".to_string(),
            },
            runtime_directory: invocation.runtime_identity.expect(
                "test runner receives a runtime identity after private directory creation",
            ),
            cgroup: None,
            session_id: Some(101),
            warning: Some("test observed-process-tree backend".to_string()),
        }
    }

    fn emit_prepared(
        invocation: &ScriptInvocation,
        observer: &mut dyn FnMut(&ScriptLifecycleEvent) -> Result<(), ActionError>,
    ) -> Result<ContainmentDescriptor, ActionError> {
        let descriptor = test_descriptor(invocation);
        observer(&ScriptLifecycleEvent::ContainmentPrepared {
            schema_version: 1,
            descriptor: descriptor.clone(),
        })?;
        Ok(descriptor)
    }

    impl ActionScriptRunner for ExecGatedCancellationRunner {
        fn run(
            &self,
            invocation: &ScriptInvocation,
            _cancellation: &dyn ActionCancellation,
            observer: &mut dyn FnMut(&ScriptLifecycleEvent) -> Result<(), ActionError>,
        ) -> Result<ScriptOutcome, ActionError> {
            let descriptor = emit_prepared(invocation, observer)?;
            observer(&ScriptLifecycleEvent::ContainmentEmpty {
                schema_version: 1,
                confidence: descriptor.confidence,
            })?;
            let output_capture = OutputCaptureSummary {
                stdout: OutputCaptureTerminal::Complete,
                stderr: OutputCaptureTerminal::Complete,
            };
            observer(&ScriptLifecycleEvent::OutputCaptureCompleted {
                schema_version: 1,
                summary: output_capture.clone(),
            })?;
            Ok(ScriptOutcome {
                status: successful_exit_status(),
                stdout_tail: Vec::new(),
                stderr_tail: Vec::new(),
                timed_out: false,
                cancelled: true,
                started: false,
                descriptor,
                containment_empty: true,
                background_descendants: false,
                output_capture,
            })
        }

        fn recover(
            &self,
            _request: &ScriptRecoveryRequest,
            _observer: &mut dyn FnMut(&ScriptLifecycleEvent) -> Result<(), ActionError>,
        ) -> Result<ScriptRecoveryOutcome, ActionError> {
            Ok(ScriptRecoveryOutcome::ContainmentAlreadyEmpty)
        }

        fn cleanup(&self, _request: &ScriptRecoveryRequest) -> Result<(), ActionError> {
            Ok(())
        }
    }

    #[cfg(unix)]
    impl ActionScriptRunner for PublicationLockProbeRunner {
        fn run(
            &self,
            invocation: &ScriptInvocation,
            _cancellation: &dyn ActionCancellation,
            observer: &mut dyn FnMut(&ScriptLifecycleEvent) -> Result<(), ActionError>,
        ) -> Result<ScriptOutcome, ActionError> {
            let (parent, component, _) = open_album_parent_capability(&self.album_dir)?;
            let publication_name = shared_album_publication_lock_name(&component);
            let (publication, publication_identity) = acquire_current_descriptor_child_lock(
                &parent,
                &publication_name,
                false,
                "manual script still holds album publication authority",
            )?;
            parent.remove_regular_child_if_identity(
                &publication_name,
                publication_identity,
            )?;
            publication.unlock()?;

            let action_name = shared_action_execution_lock_name(&component);
            let action_probe = parent.open_regular_child(&action_name, false, 0o600)?;
            match action_probe.try_lock_exclusive() {
                Err(error) if action_lock_contention(&error) => {}
                Ok(()) => {
                    let identity = metadata_for_open_file(&action_probe)?.entry_identity();
                    let _ = parent.remove_regular_child_if_identity(&action_name, identity);
                    let _ = action_probe.unlock();
                    return Err(ActionError::Conflict(
                        "manual script lost action-execution exclusion".to_string(),
                    ));
                }
                Err(error) => return Err(error.into()),
            }
            *self.probed.lock().unwrap() = true;

            let descriptor = emit_prepared(invocation, observer)?;
            observer(&ScriptLifecycleEvent::UserCodeReleased {
                schema_version: 1,
                leader: descriptor.leader.clone(),
            })?;
            observer(&ScriptLifecycleEvent::LeaderExited {
                schema_version: 1,
                raw_wait_status: 0,
            })?;
            observer(&ScriptLifecycleEvent::ContainmentEmpty {
                schema_version: 1,
                confidence: descriptor.confidence,
            })?;
            let output_capture = OutputCaptureSummary {
                stdout: OutputCaptureTerminal::Complete,
                stderr: OutputCaptureTerminal::Complete,
            };
            observer(&ScriptLifecycleEvent::OutputCaptureCompleted {
                schema_version: 1,
                summary: output_capture.clone(),
            })?;
            Ok(ScriptOutcome {
                status: successful_exit_status(),
                stdout_tail: b"ok\n".to_vec(),
                stderr_tail: Vec::new(),
                timed_out: false,
                cancelled: false,
                started: true,
                descriptor,
                containment_empty: true,
                background_descendants: false,
                output_capture,
            })
        }

        fn recover(
            &self,
            _request: &ScriptRecoveryRequest,
            _observer: &mut dyn FnMut(&ScriptLifecycleEvent) -> Result<(), ActionError>,
        ) -> Result<ScriptRecoveryOutcome, ActionError> {
            Ok(ScriptRecoveryOutcome::ContainmentAlreadyEmpty)
        }

        fn cleanup(&self, _request: &ScriptRecoveryRequest) -> Result<(), ActionError> {
            Ok(())
        }
    }

    impl ActionScriptRunner for RecordingRunner {
        fn run(
            &self,
            invocation: &ScriptInvocation,
            _cancellation: &dyn ActionCancellation,
            observer: &mut dyn FnMut(&ScriptLifecycleEvent) -> Result<(), ActionError>,
        ) -> Result<ScriptOutcome, ActionError> {
            self.calls.lock().unwrap().push(invocation.clone());
            let descriptor = emit_prepared(invocation, observer)?;
            observer(&ScriptLifecycleEvent::UserCodeReleased {
                schema_version: 1,
                leader: descriptor.leader.clone(),
            })?;
            observer(&ScriptLifecycleEvent::LeaderExited {
                schema_version: 1,
                raw_wait_status: 0,
            })?;
            observer(&ScriptLifecycleEvent::ContainmentEmpty {
                schema_version: 1,
                confidence: descriptor.confidence,
            })?;
            let output_capture = OutputCaptureSummary {
                stdout: OutputCaptureTerminal::Complete,
                stderr: OutputCaptureTerminal::Complete,
            };
            observer(&ScriptLifecycleEvent::OutputCaptureCompleted {
                schema_version: 1,
                summary: output_capture.clone(),
            })?;
            Ok(ScriptOutcome {
                status: successful_exit_status(),
                stdout_tail: b"ok\n".to_vec(),
                stderr_tail: Vec::new(),
                timed_out: false,
                cancelled: false,
                started: true,
                descriptor,
                containment_empty: true,
                background_descendants: false,
                output_capture,
            })
        }

        fn recover(
            &self,
            _request: &ScriptRecoveryRequest,
            _observer: &mut dyn FnMut(&ScriptLifecycleEvent) -> Result<(), ActionError>,
        ) -> Result<ScriptRecoveryOutcome, ActionError> {
            Ok(ScriptRecoveryOutcome::ContainmentAlreadyEmpty)
        }

        fn cleanup(&self, _request: &ScriptRecoveryRequest) -> Result<(), ActionError> {
            Ok(())
        }
    }

    #[cfg(unix)]
    fn successful_exit_status() -> ExitStatus {
        use std::os::unix::process::ExitStatusExt;
        ExitStatus::from_raw(0)
    }

    #[cfg(windows)]
    fn successful_exit_status() -> ExitStatus {
        use std::os::windows::process::ExitStatusExt;
        ExitStatus::from_raw(0)
    }

    fn test_semantics() -> ActionSemantics {
        ActionSemantics {
            wildcard_matches: crate::convert::pipeline::stages::conversion_action_wildcard_matches,
            render_template: crate::convert::pipeline::stages::conversion_action_render_template,
            sanitize_component: crate::convert::pipeline::stages::conversion_action_sanitize_component,
            fixcaps: crate::convert::renaming::capitalize_title,
            disc_number_for_path: crate::convert::pipeline::stages::conversion_action_disc_number_for_path,
        }
    }

    struct Fixture {
        _temp: tempfile::TempDir,
        output_root: PathBuf,
        album_dir: PathBuf,
        source: PathBuf,
        journal_dir: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let output_root = temp.path().join("output");
            let album_dir = output_root.join("Artist").join("Album");
            let source = temp.path().join("source.flac");
            let journal_dir = temp.path().join("coordination").join("journals");
            fs::create_dir_all(&album_dir).unwrap();
            fs::write(&source, b"source").unwrap();
            Self {
                _temp: temp,
                output_root,
                album_dir,
                source,
                journal_dir,
            }
        }

        fn context(&self, phase: ActionPhase) -> ActionContext {
            let subject_dir = match phase {
                ActionPhase::Pre => self.source.parent().unwrap().to_path_buf(),
                ActionPhase::Post => self.album_dir.clone(),
            };
            let mut tokens = BTreeMap::new();
            tokens.insert("ARTIST".to_string(), "Nobody's Artist".to_string());
            tokens.insert("ALBUM".to_string(), "Nobody's Perfect".to_string());
            tokens.insert("TITLE_EXTRA".to_string(), "Japan SHM".to_string());
            tokens.insert("YEAR".to_string(), "1988".to_string());
            tokens.insert("FORMAT".to_string(), "flac".to_string());
            ActionContext {
                run_identity: "run-1".to_string(),
                album_identity: "album-1".to_string(),
                phase,
                subject_dir,
                source_path: self.source.clone(),
                source_is_directory: false,
                output_root: self.output_root.clone(),
                album_dir: self.album_dir.clone(),
                environment_album_dir: None,
                retained_album_capability: None,
                retained_output_capability: None,
                retained_journal_capability: None,
                coordination_io_dir: None,
                protected_sources: [self.source.clone()].into_iter().collect(),
                protected_generated_paths: [self.album_dir.join("conversion.log")]
                    .into_iter()
                    .collect(),
                album_tokens: tokens,
                disc_count: Some(2),
                journal_dir: self.journal_dir.clone(),
                batch_source_scope_root: None,
                explicit_scope: false,
                semantics: test_semantics(),
            }
        }
    }

    fn targeting(patterns: &[&str]) -> TargetSpec {
        TargetSpec {
            target: patterns.iter().map(|value| (*value).to_string()).collect(),
            ..TargetSpec::default()
        }
    }

    fn copy_action(destination: PathBuf) -> ConversionAction {
        ConversionAction::Copy(CopyAction {
            targeting: targeting(&["track.flac"]),
            destination,
        })
    }

    #[test]
    fn automatic_post_phase_claims_admit_opposite_order_destinations_atomically() {
        let _coordination = crate::concurrency::scoped_test_coordination_root();
        let fixture = Fixture::new();
        fs::write(fixture.album_dir.join("track.flac"), b"track").unwrap();
        let x = fixture._temp.path().join("external-x");
        let y = fixture._temp.path().join("external-y");
        let z = fixture._temp.path().join("external-z");
        let w = fixture._temp.path().join("external-w");
        let context = fixture.context(ActionPhase::Post);

        let first = ActionPipeline {
            pre: Vec::new(),
            post: vec![copy_action(x.clone()), copy_action(y.clone())],
        };
        let opposite = ActionPipeline {
            pre: Vec::new(),
            post: vec![copy_action(y.clone()), copy_action(x.clone())],
        };
        let disjoint = ActionPipeline {
            pre: Vec::new(),
            post: vec![copy_action(z.clone()), copy_action(w.clone())],
        };

        let first_claims = remove_covered_phase_claims(
            shared_path_claims_for_configured_action_phase(&first, &context).unwrap(),
            &[],
        );
        let _first_guard =
            crate::concurrency::MutationClaimGuard::acquire_ephemeral(first_claims).unwrap();

        let opposite_claims = remove_covered_phase_claims(
            shared_path_claims_for_configured_action_phase(&opposite, &context).unwrap(),
            &[],
        );
        let error = crate::concurrency::MutationClaimGuard::acquire_ephemeral(opposite_claims)
            .expect_err("opposite X/Y phase must lose before any action mutation");
        assert!(error.contains("live owner"));
        assert!(!x.exists());
        assert!(!y.exists());

        let disjoint_claims = remove_covered_phase_claims(
            shared_path_claims_for_configured_action_phase(&disjoint, &context).unwrap(),
            &[],
        );
        let _disjoint_guard = crate::concurrency::MutationClaimGuard::acquire_ephemeral(disjoint_claims)
            .expect("disjoint action destinations must remain concurrent");
    }

    #[test]
    fn automatic_phase_admission_registers_one_complete_execution_claim_before_actions() {
        let _coordination = crate::concurrency::scoped_test_coordination_root();
        let fixture = Fixture::new();
        fs::write(fixture.album_dir.join("track.flac"), b"track").unwrap();
        let x = fixture._temp.path().join("runtime-x");
        let y = fixture._temp.path().join("runtime-y");
        let context = fixture.context(ActionPhase::Post);
        let first = ActionPipeline {
            pre: Vec::new(),
            post: vec![copy_action(x.clone()), copy_action(y.clone())],
        };
        let opposite = ActionPipeline {
            pre: Vec::new(),
            post: vec![copy_action(y.clone()), copy_action(x.clone())],
        };
        let z = fixture._temp.path().join("runtime-z");
        let w = fixture._temp.path().join("runtime-w");
        let disjoint = ActionPipeline {
            pre: Vec::new(),
            post: vec![copy_action(z), copy_action(w)],
        };

        let first_item = format!("action-phase-first-{}", uuid::Uuid::new_v4());
        let first_execution = uuid::Uuid::new_v4();
        let first_queue = Arc::new(
            crate::concurrency::PersistentLease::create(
                crate::concurrency::LeaseFamily::QueueExecution {
                    execution_id: first_execution,
                },
                &[],
            )
            .unwrap(),
        );
        let first_queue_path = first_queue.descriptor_path().to_path_buf();
        crate::concurrency::register_runtime_execution(
            &first_item,
            first_execution,
            Arc::clone(&first_queue),
            None,
        )
        .unwrap();

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(crate::concurrency::with_runtime_execution_scope(
            first_item.clone(),
            async { admit_conversion_action_phase_claims(&first, &context, false) },
        ))
        .unwrap();

        let held = crate::concurrency::runtime_execution_claims(&first_item).unwrap();
        for required in shared_path_claims_for_configured_action_phase(&first, &context).unwrap() {
            assert!(held.iter().any(|claim| claim.covers(&required)));
        }

        let second_item = format!("action-phase-second-{}", uuid::Uuid::new_v4());
        let second_execution = uuid::Uuid::new_v4();
        let second_queue = Arc::new(
            crate::concurrency::PersistentLease::create(
                crate::concurrency::LeaseFamily::QueueExecution {
                    execution_id: second_execution,
                },
                &[],
            )
            .unwrap(),
        );
        let second_queue_path = second_queue.descriptor_path().to_path_buf();
        crate::concurrency::register_runtime_execution(
            &second_item,
            second_execution,
            Arc::clone(&second_queue),
            None,
        )
        .unwrap();
        let error = runtime
            .block_on(crate::concurrency::with_runtime_execution_scope(
                second_item.clone(),
                async { admit_conversion_action_phase_claims(&opposite, &context, false) },
            ))
            .expect_err("opposite runtime phase must lose complete admission");
        assert!(error.to_string().contains("live owner"));
        assert!(!x.exists());
        assert!(!y.exists());

        runtime
            .block_on(crate::concurrency::with_runtime_execution_scope(
                second_item.clone(),
                async { admit_conversion_action_phase_claims(&disjoint, &context, false) },
            ))
            .expect("disjoint runtime action phase must remain concurrent");

        crate::concurrency::unregister_runtime_execution(&second_item);
        crate::concurrency::unregister_runtime_execution(&first_item);
        drop(second_queue);
        drop(first_queue);
        let _ = std::fs::remove_file(second_queue_path);
        let _ = std::fs::remove_file(first_queue_path);
    }

    #[test]
    fn automatic_pre_phase_claims_admit_complete_create_folder_set() {
        let _coordination = crate::concurrency::scoped_test_coordination_root();
        let fixture = Fixture::new();
        let x = fixture._temp.path().join("pre-x");
        let y = fixture._temp.path().join("pre-y");
        let mut context = fixture.context(ActionPhase::Pre);
        context.source_is_directory = true;
        let first = ActionPipeline {
            pre: vec![
                ConversionAction::CreateFolder(CreateFolderAction {
                    path: x.clone(),
                    continue_on_error: false,
                }),
                ConversionAction::CreateFolder(CreateFolderAction {
                    path: y.clone(),
                    continue_on_error: false,
                }),
            ],
            post: Vec::new(),
        };
        let opposite = ActionPipeline {
            pre: vec![
                ConversionAction::CreateFolder(CreateFolderAction {
                    path: y.clone(),
                    continue_on_error: false,
                }),
                ConversionAction::CreateFolder(CreateFolderAction {
                    path: x.clone(),
                    continue_on_error: false,
                }),
            ],
            post: Vec::new(),
        };
        let _first_guard = crate::concurrency::MutationClaimGuard::acquire_ephemeral(
            shared_path_claims_for_configured_action_phase(&first, &context).unwrap(),
        )
        .unwrap();
        assert!(crate::concurrency::MutationClaimGuard::acquire_ephemeral(
            shared_path_claims_for_configured_action_phase(&opposite, &context).unwrap(),
        )
        .is_err());
        assert!(!x.exists());
        assert!(!y.exists());
    }

    #[test]
    fn phase_claims_already_inside_album_write_capability_are_not_republished() {
        let _coordination = crate::concurrency::scoped_test_coordination_root();
        let fixture = Fixture::new();
        let context = fixture.context(ActionPhase::Post);
        let pipeline = ActionPipeline {
            pre: Vec::new(),
            post: vec![ConversionAction::CreateFolder(CreateFolderAction {
                path: PathBuf::from("Artwork"),
                continue_on_error: false,
            })],
        };
        let album_claim = crate::concurrency::PathClaim::resolve(
            &fixture.album_dir,
            crate::concurrency::ClaimMode::Write,
            crate::concurrency::ClaimScope::Subtree,
        )
        .unwrap();
        let retained = remove_covered_phase_claims(
            shared_path_claims_for_configured_action_phase(&pipeline, &context).unwrap(),
            &[album_claim],
        );
        assert!(retained.is_empty());
    }

    fn engine<'a>(runner: &'a dyn ActionScriptRunner) -> ActionEngine<'a> {
        let filesystem: &'static CapabilityActionFilesystem =
            Box::leak(Box::new(CapabilityActionFilesystem::new()));
        ActionEngine {
            filesystem,
            scripts: runner,
        }
    }

    fn operation_targets(plan: &ActionPlan) -> Vec<PathBuf> {
        plan.operations
            .iter()
            .map(|operation| match operation {
                PlannedOperation::Rename { source, .. } => source.clone(),
                PlannedOperation::Copy { source, .. }
                | PlannedOperation::RepairCopyMetadata { source, .. } => source.clone(),
                PlannedOperation::Move { source, .. } => source.clone(),
                PlannedOperation::Delete { target, .. } => target.clone(),
                PlannedOperation::CreateDirectory { path } => path.clone(),
                PlannedOperation::RunScript { script, .. } => script.clone(),
            })
            .collect()
    }

    #[cfg(unix)]
    fn copy_test_metadata(source: &Path, destination: &Path) {
        use std::os::unix::fs::PermissionsExt;

        let source_metadata = fs::metadata(source).expect("source metadata");
        fs::set_permissions(
            destination,
            fs::Permissions::from_mode(source_metadata.permissions().mode()),
        )
        .expect("copy source mode");
        let modified = source_metadata.modified().expect("source mtime");
        let file = File::options()
            .read(true)
            .write(source_metadata.is_file())
            .open(destination)
            .expect("open copied object for timestamp repair");
        file.set_times(fs::FileTimes::new().set_modified(modified))
            .expect("copy source mtime");
    }

    #[cfg(unix)]
    #[test]
    fn copy_zero_operation_precondition_rejects_in_place_source_change() {
        let fixture = Fixture::new();
        let source = fixture.album_dir.join("eac.log");
        let destination_root = fixture.album_dir.join("collected");
        let destination = destination_root.join("eac.log");
        fs::create_dir_all(&destination_root).unwrap();
        fs::write(&source, b"aaaa").unwrap();
        fs::copy(&source, &destination).unwrap();
        copy_test_metadata(&source, &destination);

        let filesystem = CapabilityActionFilesystem::new();
        let runner = RecordingRunner::default();
        let action_engine = ActionEngine {
            filesystem: &filesystem,
            scripts: &runner,
        };
        let action = ConversionAction::Copy(CopyAction {
            targeting: targeting(&["eac.log"]),
            destination: PathBuf::from("collected"),
        });
        let context = fixture.context(ActionPhase::Post);
        let plan = action_engine.preview_action(0, &action, &context).unwrap();
        assert!(plan.operations.is_empty());
        assert_eq!(plan.planning_preconditions.len(), 1);
        let graph = preview_operand_graph(&filesystem, std::slice::from_ref(&plan)).unwrap();
        let roles = graph
            .objects
            .iter()
            .flat_map(|object| object.paths.iter())
            .flat_map(|path| path.roles.iter())
            .cloned()
            .collect::<BTreeSet<_>>();
        assert!(roles.contains("copy_noop_source"));
        assert!(roles.contains("copy_noop_destination"));

        fs::write(&source, b"bbbb").unwrap();
        assert!(matches!(
            validate_planning_preconditions(&filesystem, &[plan]),
            Err(ActionError::PreviewStale(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn copy_zero_operation_precondition_rejects_in_place_destination_change() {
        let fixture = Fixture::new();
        let source = fixture.album_dir.join("eac.log");
        let destination_root = fixture.album_dir.join("collected");
        let destination = destination_root.join("eac.log");
        fs::create_dir_all(&destination_root).unwrap();
        fs::write(&source, b"aaaa").unwrap();
        fs::copy(&source, &destination).unwrap();
        copy_test_metadata(&source, &destination);

        let filesystem = CapabilityActionFilesystem::new();
        let runner = RecordingRunner::default();
        let action_engine = ActionEngine {
            filesystem: &filesystem,
            scripts: &runner,
        };
        let action = ConversionAction::Copy(CopyAction {
            targeting: targeting(&["eac.log"]),
            destination: PathBuf::from("collected"),
        });
        let context = fixture.context(ActionPhase::Post);
        let plan = action_engine.preview_action(0, &action, &context).unwrap();
        assert!(plan.operations.is_empty());

        fs::write(&destination, b"bbbb").unwrap();
        assert!(matches!(
            validate_planning_preconditions(&filesystem, &[plan]),
            Err(ActionError::PreviewStale(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn directory_copy_noop_binds_each_tree_metadata_entry() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = Fixture::new();
        let source = fixture.album_dir.join("booklet");
        let source_child = source.join("page.txt");
        let destination_root = fixture.album_dir.join("collected");
        let destination = destination_root.join("booklet");
        let destination_child = destination.join("page.txt");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&destination).unwrap();
        fs::write(&source_child, b"page").unwrap();
        fs::set_permissions(&source_child, fs::Permissions::from_mode(0o644)).unwrap();
        fs::copy(&source_child, &destination_child).unwrap();

        let filesystem = CapabilityActionFilesystem::new();
        let runner = RecordingRunner::default();
        let action_engine = ActionEngine {
            filesystem: &filesystem,
            scripts: &runner,
        };
        let context = fixture.context(ActionPhase::Post);
        prepare_context_capabilities(&filesystem, &context).unwrap();
        filesystem
            .repair_copy_metadata(&source, &destination, true)
            .unwrap();
        let action = ConversionAction::Copy(CopyAction {
            targeting: targeting(&["booklet"]),
            destination: PathBuf::from("collected"),
        });
        let plan = action_engine.preview_action(0, &action, &context).unwrap();
        assert!(plan.operations.is_empty());

        fs::set_permissions(&source_child, fs::Permissions::from_mode(0o600)).unwrap();
        fs::set_permissions(&destination_child, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(matches!(
            validate_planning_preconditions(&filesystem, &[plan]),
            Err(ActionError::PreviewStale(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn copy_mutation_preflight_rejects_reviewed_source_metadata_change() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = Fixture::new();
        let source = fixture.album_dir.join("eac.log");
        fs::write(&source, b"reviewed payload").unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o644)).unwrap();
        let filesystem = CapabilityActionFilesystem::new();
        let context = fixture.context(ActionPhase::Post);
        prepare_context_capabilities(&filesystem, &context).unwrap();
        let reviewed = filesystem.identity(&source, true).unwrap();

        fs::set_permissions(&source, fs::Permissions::from_mode(0o600)).unwrap();

        assert!(matches!(
            verify_same_copy_source(&filesystem, &source, &reviewed),
            Err(ActionError::Contradiction(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn copy_metadata_mismatch_is_journaled_and_repaired() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = Fixture::new();
        let source = fixture.album_dir.join("eac.log");
        let destination_root = fixture.album_dir.join("collected");
        let destination = destination_root.join("eac.log");
        fs::create_dir_all(&destination_root).unwrap();
        fs::write(&source, b"identical payload").unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o644)).unwrap();
        fs::copy(&source, &destination).unwrap();
        copy_test_metadata(&source, &destination);
        fs::set_permissions(&destination, fs::Permissions::from_mode(0o600)).unwrap();

        let filesystem = CapabilityActionFilesystem::new();
        let runner = RecordingRunner::default();
        let action_engine = ActionEngine {
            filesystem: &filesystem,
            scripts: &runner,
        };
        let pipeline = ActionPipeline {
            pre: Vec::new(),
            post: vec![ConversionAction::Copy(CopyAction {
                targeting: targeting(&["eac.log"]),
                destination: PathBuf::from("collected"),
            })],
        };
        let context = fixture.context(ActionPhase::Post);
        let plan = action_engine.preview_phase(&pipeline, &context).unwrap();
        assert!(matches!(
            plan[0].operations.as_slice(),
            [PlannedOperation::RepairCopyMetadata { .. }]
        ));

        let report = action_engine
            .execute_phase(&pipeline, &context, &NeverCancel)
            .expect("metadata repair execution");
        assert!(!report.has_errors());
        let source_identity = filesystem.identity(&source, true).unwrap();
        let destination_identity = filesystem.identity(&destination, true).unwrap();
        assert!(destination_identity.copy_state_equivalent(&source_identity));

        let rerun = action_engine.preview_phase(&pipeline, &context).unwrap();
        assert!(rerun[0].operations.is_empty());
        assert_eq!(rerun[0].planning_preconditions.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn directory_copy_metadata_repair_covers_descendant_state() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = Fixture::new();
        let source = fixture.album_dir.join("booklet");
        let source_child = source.join("page.txt");
        let destination_root = fixture.album_dir.join("collected");
        let destination = destination_root.join("booklet");
        let destination_child = destination.join("page.txt");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&destination).unwrap();
        fs::write(&source_child, b"page").unwrap();
        fs::copy(&source_child, &destination_child).unwrap();

        let filesystem = CapabilityActionFilesystem::new();
        let runner = RecordingRunner::default();
        let action_engine = ActionEngine {
            filesystem: &filesystem,
            scripts: &runner,
        };
        let context = fixture.context(ActionPhase::Post);
        prepare_context_capabilities(&filesystem, &context).unwrap();
        filesystem
            .repair_copy_metadata(&source, &destination, true)
            .expect("canonicalize initial copied tree");
        fs::set_permissions(&destination_child, fs::Permissions::from_mode(0o600)).unwrap();

        let pipeline = ActionPipeline {
            pre: Vec::new(),
            post: vec![ConversionAction::Copy(CopyAction {
                targeting: targeting(&["booklet"]),
                destination: PathBuf::from("collected"),
            })],
        };
        let plan = action_engine.preview_phase(&pipeline, &context).unwrap();
        assert!(matches!(
            plan[0].operations.as_slice(),
            [PlannedOperation::RepairCopyMetadata { .. }]
        ));
        action_engine
            .execute_phase(&pipeline, &context, &NeverCancel)
            .expect("directory metadata repair");
        let source_identity = filesystem.identity(&source, true).unwrap();
        let destination_identity = filesystem.identity(&destination, true).unwrap();
        assert!(destination_identity.copy_state_equivalent(&source_identity));
    }

    #[test]
    fn matcher_tree_scan_checks_cancellation_during_enumeration() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct CancelAfter {
            checks: AtomicUsize,
            limit: usize,
        }

        impl ActionCancellation for CancelAfter {
            fn is_cancelled(&self) -> bool {
                self.checks.fetch_add(1, Ordering::SeqCst) >= self.limit
            }
        }

        #[derive(Default)]
        struct Progress {
            updates: AtomicUsize,
        }

        impl ExplicitPreviewProgressObserver for Progress {
            fn update(&self, _phase: &'static str, _completed: u64, _total: Option<u64>) {
                self.updates.fetch_add(1, Ordering::SeqCst);
            }
        }

        let fixture = Fixture::new();
        for index in 0..256 {
            fs::write(
                fixture.album_dir.join(format!("entry-{index:03}.log")),
                b"metadata-only",
            )
            .unwrap();
        }
        let filesystem = CapabilityActionFilesystem::new();
        let mut context = fixture.context(ActionPhase::Post);
        context.explicit_scope = true;
        context.journal_dir = fixture.album_dir.join(".tonepoet-actions-manual");
        prepare_context_capabilities(&filesystem, &context).unwrap();
        let cancellation = CancelAfter {
            checks: AtomicUsize::new(0),
            limit: 8,
        };
        let progress = Progress::default();

        assert!(matches!(
            capture_preview_matcher_tree_cancellable(
                &filesystem,
                &context,
                &cancellation,
                &progress,
            ),
            Err(ActionError::CancelledBeforeMutation(_))
        ));
        assert!(
            progress.updates.load(Ordering::SeqCst) < 256,
            "cancellation must interrupt the traversal before the whole tree is materialized"
        );
    }

    #[test]
    fn preview_operand_graph_merges_source_and_destination_aliases_by_object_identity() {
        let fixture = Fixture::new();
        let source = fixture.album_dir.join("foo.log");
        let destination = fixture.album_dir.join("FOO.log");
        fs::write(&source, b"log").unwrap();
        fs::hard_link(&source, &destination).unwrap();
        let filesystem = CapabilityActionFilesystem::new();
        let context = fixture.context(ActionPhase::Post);
        prepare_context_capabilities(&filesystem, &context).unwrap();
        let expected_source = filesystem.identity(&source, true).unwrap();
        let plan = ActionPlan {
            action_kind: "rename".to_string(),
            operations: vec![PlannedOperation::Rename {
                source: source.clone(),
                destination: destination.clone(),
                staging: fixture.album_dir.join(".rename-stage"),
                expected_staged: expected_source.clone(),
                expected_source,
            }],
            planning_preconditions: Vec::new(),
            notices: Vec::new(),
        };

        let graph = preview_operand_graph(&filesystem, &[plan]).unwrap();
        assert_eq!(graph.objects.len(), 1, "hard-link/case aliases are one object");
        assert_eq!(graph.objects[0].paths.len(), 2);
        assert!(graph.absent_paths.iter().all(|entry| {
            entry.path != lexical_normalize(&destination)
        }));
        validate_preview_operand_graph(&filesystem, &graph).unwrap();
    }

    #[test]
    fn preview_operand_graph_models_rename_chains_without_false_absence() {
        let fixture = Fixture::new();
        let first = fixture.album_dir.join("a.log");
        let second = fixture.album_dir.join("b.log");
        let third = fixture.album_dir.join("c.log");
        fs::write(&first, b"a").unwrap();
        fs::write(&second, b"b").unwrap();
        let filesystem = CapabilityActionFilesystem::new();
        let context = fixture.context(ActionPhase::Post);
        prepare_context_capabilities(&filesystem, &context).unwrap();
        let first_identity = filesystem.identity(&first, true).unwrap();
        let second_identity = filesystem.identity(&second, true).unwrap();
        let plans = vec![ActionPlan {
            action_kind: "rename".to_string(),
            operations: vec![
                PlannedOperation::Rename {
                    source: first.clone(),
                    destination: second.clone(),
                    staging: fixture.album_dir.join(".stage-a"),
                    expected_source: first_identity.clone(),
                    expected_staged: first_identity,
                },
                PlannedOperation::Rename {
                    source: second.clone(),
                    destination: third.clone(),
                    staging: fixture.album_dir.join(".stage-b"),
                    expected_source: second_identity.clone(),
                    expected_staged: second_identity,
                },
            ],
            planning_preconditions: Vec::new(),
            notices: Vec::new(),
        }];

        let graph = preview_operand_graph(&filesystem, &plans).unwrap();
        assert!(graph.absent_paths.iter().all(|entry| {
            entry.path != lexical_normalize(&second)
        }));
        assert!(graph.absent_paths.iter().any(|entry| {
            entry.path == lexical_normalize(&third)
        }));
        validate_preview_operand_graph(&filesystem, &graph).unwrap();
    }

    #[test]
    fn preview_operand_graph_models_rename_cycle_as_two_existing_objects() {
        let fixture = Fixture::new();
        let first = fixture.album_dir.join("a.log");
        let second = fixture.album_dir.join("b.log");
        fs::write(&first, b"a").unwrap();
        fs::write(&second, b"b").unwrap();
        let filesystem = CapabilityActionFilesystem::new();
        let context = fixture.context(ActionPhase::Post);
        prepare_context_capabilities(&filesystem, &context).unwrap();
        let first_identity = filesystem.identity(&first, true).unwrap();
        let second_identity = filesystem.identity(&second, true).unwrap();
        let plans = vec![ActionPlan {
            action_kind: "rename".to_string(),
            operations: vec![
                PlannedOperation::Rename {
                    source: first.clone(),
                    destination: second.clone(),
                    staging: fixture.album_dir.join(".stage-a"),
                    expected_source: first_identity.clone(),
                    expected_staged: first_identity,
                },
                PlannedOperation::Rename {
                    source: second.clone(),
                    destination: first.clone(),
                    staging: fixture.album_dir.join(".stage-b"),
                    expected_source: second_identity.clone(),
                    expected_staged: second_identity,
                },
            ],
            planning_preconditions: Vec::new(),
            notices: Vec::new(),
        }];

        let graph = preview_operand_graph(&filesystem, &plans).unwrap();
        assert_eq!(graph.objects.len(), 2);
        assert!(graph.absent_paths.iter().all(|entry| {
            entry.path != lexical_normalize(&first)
                && entry.path != lexical_normalize(&second)
        }));
        validate_preview_operand_graph(&filesystem, &graph).unwrap();
    }

    #[test]
    fn matcher_tree_does_not_hash_unrelated_audio_payloads() {
        let fixture = Fixture::new();
        let audio = fixture.album_dir.join("unrelated.flac");
        fs::write(&audio, vec![0_u8; 1024 * 1024]).unwrap();
        let filesystem = CapabilityActionFilesystem::new();
        let mut context = fixture.context(ActionPhase::Post);
        context.explicit_scope = true;
        context.journal_dir = fixture.album_dir.join(".tonepoet-actions-manual");
        prepare_context_capabilities(&filesystem, &context).unwrap();

        let before = capture_preview_matcher_tree(&filesystem, &context).unwrap();
        fs::write(&audio, vec![1_u8; 1024 * 1024]).unwrap();
        let after = capture_preview_matcher_tree(&filesystem, &context).unwrap();
        assert_eq!(
            before, after,
            "matcher snapshot is directory-entry metadata only; concrete operands carry content identity"
        );
    }

    #[cfg(unix)]
    #[test]
    fn reviewed_runscript_content_replacement_is_rejected() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = Fixture::new();
        let script = fixture._temp.path().join("action.sh");
        fs::write(&script, b"#!/bin/sh\nexit 0\n").unwrap();
        let mut permissions = fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).unwrap();
        let filesystem = CapabilityActionFilesystem::new();
        let context = fixture.context(ActionPhase::Post);
        prepare_context_capabilities(&filesystem, &context).unwrap();
        let expected_script = filesystem.identity(&script, true).unwrap();
        let plan = ActionPlan {
            action_kind: "runscript".to_string(),
            operations: vec![PlannedOperation::RunScript {
                script: script.clone(),
                expected_script,
                args: Vec::new(),
                working_directory: fixture.album_dir.clone(),
                environment: BTreeMap::new(),
                timeout_seconds: 30,
                runtime_directory: fixture.journal_dir.join("runtime"),
                containment_token: "token".to_string(),
            }],
            planning_preconditions: Vec::new(),
            notices: Vec::new(),
        };
        let graph = preview_operand_graph(&filesystem, &[plan]).unwrap();

        let replacement = fixture._temp.path().join("replacement.sh");
        fs::write(&replacement, b"#!/bin/sh\nexit 42\n").unwrap();
        let mut replacement_permissions = fs::metadata(&replacement).unwrap().permissions();
        replacement_permissions.set_mode(0o755);
        fs::set_permissions(&replacement, replacement_permissions).unwrap();
        fs::rename(&replacement, &script).unwrap();

        assert!(matches!(
            validate_preview_operand_graph(&filesystem, &graph),
            Err(ActionError::PreviewStale(_))
        ));
    }

    #[test]
    fn unknown_action_type_fails_loudly() {
        let error = serde_json::from_str::<ConversionAction>(r#"{"type":"teleport"}"#)
            .expect_err("unknown tagged variant must fail");
        assert!(error.to_string().contains("unknown variant"));
    }

    #[test]
    fn sr1_wildcards_ignore_hidden_and_generated_files_but_exact_target_is_escape_hatch() {
        let fixture = Fixture::new();
        fs::write(fixture.album_dir.join("eac.log"), b"eac").unwrap();
        fs::write(fixture.album_dir.join("conversion.log"), b"generated").unwrap();
        fs::write(fixture.album_dir.join(".conversion-log-finalization.lock"), b"lock").unwrap();
        fs::create_dir(fixture.album_dir.join(".tonepoet-batch")).unwrap();
        fs::write(fixture.album_dir.join(".tonepoet-batch").join("hidden.log"), b"hidden").unwrap();
        let runner = RecordingRunner::default();
        let engine = engine(&runner);
        let wildcard = ConversionAction::Delete(DeleteAction {
            targeting: targeting(&["*.log"]),
        });
        let plan = engine
            .preview_action(0, &wildcard, &fixture.context(ActionPhase::Post))
            .unwrap();
        assert_eq!(operation_targets(&plan), vec![fixture.album_dir.join("eac.log")]);

        let exact = ConversionAction::Delete(DeleteAction {
            targeting: targeting(&["conversion.log"]),
        });
        let plan = engine
            .preview_action(0, &exact, &fixture.context(ActionPhase::Post))
            .unwrap();
        assert_eq!(operation_targets(&plan), vec![fixture.album_dir.join("conversion.log")]);
    }

    #[test]
    fn exact_target_cannot_delete_manual_recovery_authority() {
        let fixture = Fixture::new();
        let manual_root = fixture.album_dir.join(".tonepoet-actions-manual");
        fs::create_dir_all(&manual_root).unwrap();
        let mut context = fixture.context(ActionPhase::Post);
        context.journal_dir = manual_root;
        let runner = RecordingRunner::default();
        let action = ConversionAction::Delete(DeleteAction {
            targeting: targeting(&[".tonepoet-actions-manual"]),
        });
        let error = engine(&runner)
            .preview_action(0, &action, &context)
            .expect_err("recovery authority must never be an action target");
        assert!(matches!(error, ActionError::UnsafePath(_)));
    }

    #[test]
    fn exact_include_does_not_override_wildcard_exclude() {
        let fixture = Fixture::new();
        fs::write(fixture.album_dir.join("foo.log"), b"log").unwrap();
        let runner = RecordingRunner::default();
        let action = ConversionAction::Delete(DeleteAction {
            targeting: TargetSpec {
                target: vec!["foo.log".to_string()],
                exclude: vec!["*.log".to_string()],
                ..TargetSpec::default()
            },
        });
        let plan = engine(&runner)
            .preview_action(0, &action, &fixture.context(ActionPhase::Post))
            .unwrap();
        assert!(
            plan.operations.is_empty(),
            "wildcard excludes must veto exact includes for destructive actions"
        );
    }

    #[test]
    fn absent_manual_authority_root_is_not_unresolved_state() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing-manual-authority");
        assert!(!workspace_has_unresolved_action_state(&missing));
    }

    #[cfg(unix)]
    #[test]
    fn explicit_album_lock_survives_album_directory_replacement() {
        let temp = tempfile::tempdir().unwrap();
        let album = temp.path().join("album");
        let retired = temp.path().join("album.retired");
        fs::create_dir(&album).unwrap();

        let first = acquire_explicit_action_run_lock_for_album(&album).unwrap();
        fs::rename(&album, &retired).unwrap();
        fs::create_dir(&album).unwrap();

        let error = acquire_explicit_action_run_lock_for_album(&album)
            .expect_err("a replacement album must remain serialized by the stable parent lock");
        assert!(matches!(error, ActionError::Conflict(_)));
        assert!(
            !album.join(".tonepoet-actions-manual/.manual-run.lock").exists(),
            "new releases must not create an in-album lock inode"
        );

        drop(first);
        acquire_explicit_action_run_lock_for_album(&album)
            .expect("the replacement album becomes available after the original authority drops");
    }

    #[test]
    fn manual_authority_creates_no_output_tree_lock_artifacts() {
        let temp = tempfile::tempdir().unwrap();
        let album = temp.path().join("album");
        fs::create_dir(&album).unwrap();

        let lock = acquire_explicit_action_run_lock_for_album(&album).unwrap();
        assert!(!temp.path().join(".tonepoet-action-lock-registry.lock").exists());
        assert!(!temp.path().join(".tonepoet-action-locks").exists());
        assert!(!album.join(".tonepoet-actions-manual/.manual-run.lock").exists());
        drop(lock);

        acquire_explicit_action_run_lock_for_album(&album)
            .expect("shared publication authority must be reusable after unlock");
    }

    #[test]
    fn shared_publication_lock_is_removed_after_unlock() {
        let temp = tempfile::tempdir().unwrap();
        let album = temp.path().join("album");
        fs::create_dir(&album).unwrap();
        let path = shared_album_publication_lock_display_path_for_test(&album).unwrap();

        let lock = acquire_explicit_action_run_lock_for_album(&album).unwrap();
        assert!(path.is_file());
        drop(lock);
        assert!(!path.exists(), "ordinary publication authority must not leave an artifact");
        acquire_explicit_action_run_lock_for_album(&album).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn publication_transition_lock_can_be_released_while_action_authority_remains() {
        let temp = tempfile::tempdir().unwrap();
        let album = temp.path().join("album");
        fs::create_dir(&album).unwrap();
        let (parent, component, _) = open_album_parent_capability(&album).unwrap();
        let publication_name = shared_album_publication_lock_name(&component);
        let action_name = shared_action_execution_lock_name(&component);

        let mut authority = acquire_explicit_action_run_lock_for_album(&album).unwrap();
        assert!(authority.holds_action_execution_authority());
        assert!(parent.entry_identity(&publication_name).unwrap().is_some());
        assert!(parent.entry_identity(&action_name).unwrap().is_some());

        authority.release_publication_authority();
        assert!(
            parent.entry_identity(&publication_name).unwrap().is_none(),
            "the album publication lock entry must be gone before actions execute"
        );
        assert!(
            parent.entry_identity(&action_name).unwrap().is_some(),
            "the independent action-execution authority must remain"
        );

        let publication_probe = parent
            .open_regular_child(&publication_name, true, 0o600)
            .unwrap();
        publication_probe
            .try_lock_exclusive()
            .expect("a script-time probe must be able to acquire the publication lock");
        let publication_identity = metadata_for_open_file(&publication_probe)
            .unwrap()
            .entry_identity();
        parent
            .remove_regular_child_if_identity(&publication_name, publication_identity)
            .unwrap();
        publication_probe.unlock().unwrap();

        let action_probe = parent.open_regular_child(&action_name, false, 0o600).unwrap();
        let error = action_probe
            .try_lock_exclusive()
            .expect_err("manual actions must still be excluded during automatic post actions");
        assert!(action_lock_contention(&error));
        drop(action_probe);

        drop(authority);
        assert!(parent.entry_identity(&action_name).unwrap().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn publication_lock_waiter_wakes_on_an_orphaned_non_authoritative_inode() {
        use std::sync::mpsc;

        let temp = tempfile::tempdir().unwrap();
        let album = temp.path().join("album");
        fs::create_dir(&album).unwrap();
        let (parent, component, _) = open_album_parent_capability(&album).unwrap();
        let publication_name = shared_album_publication_lock_name(&component);
        let mut authority = acquire_explicit_action_run_lock_for_album(&album).unwrap();

        let waiter = parent
            .open_regular_child(&publication_name, false, 0o600)
            .unwrap();
        let waiter_identity = metadata_for_open_file(&waiter).unwrap().entry_identity();
        let (ready_tx, ready_rx) = mpsc::channel();
        let (acquired_tx, acquired_rx) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            ready_tx.send(()).unwrap();
            waiter.lock_exclusive().unwrap();
            acquired_tx.send(waiter).unwrap();
        });
        ready_rx.recv().unwrap();

        authority.release_publication_authority();
        let orphaned_waiter = acquired_rx.recv().unwrap();
        assert_ne!(
            parent.entry_identity(&publication_name).unwrap(),
            Some(waiter_identity),
            "a waiter on the removed inode must fail the current-entry identity test"
        );

        let current = parent
            .open_regular_child(&publication_name, true, 0o600)
            .unwrap();
        current
            .try_lock_exclusive()
            .expect("a current lock inode can be acquired while the orphaned inode is locked");
        let current_identity = metadata_for_open_file(&current).unwrap().entry_identity();
        parent
            .remove_regular_child_if_identity(&publication_name, current_identity)
            .unwrap();
        current.unlock().unwrap();
        orphaned_waiter.unlock().unwrap();
        thread.join().unwrap();
        drop(authority);
    }

    #[cfg(unix)]
    #[test]
    fn unrelated_albums_in_one_output_root_keep_independent_manual_authority() {
        let temp = tempfile::tempdir().unwrap();
        let first_album = temp.path().join("first");
        let second_album = temp.path().join("second");
        fs::create_dir(&first_album).unwrap();
        fs::create_dir(&second_album).unwrap();

        let first = acquire_explicit_action_run_lock_for_album(&first_album).unwrap();
        let second = acquire_explicit_action_run_lock_for_album(&second_album)
            .expect("per-album authority must not serialize unrelated publications");
        drop(second);
        drop(first);
    }

    #[cfg(unix)]
    #[test]
    fn acquiring_manual_authority_does_not_materialize_the_album_or_journal_root() {
        let temp = tempfile::tempdir().unwrap();
        let album = temp.path().join("not-yet-published");
        let lock = acquire_explicit_action_run_lock_for_album(&album).unwrap();
        assert!(!album.exists());
        assert!(!album.join(".tonepoet-actions-manual").exists());
        drop(lock);
    }

    #[cfg(unix)]
    #[test]
    fn publication_preflight_refuses_unresolved_manual_recovery_authority() {
        let temp = tempfile::tempdir().unwrap();
        let album = temp.path().join("album");
        let manual_root = album.join(".tonepoet-actions-manual");
        fs::create_dir_all(&manual_root).unwrap();
        fs::write(manual_root.join("actions-post.write-tmp"), b"unresolved").unwrap();

        let lock = acquire_explicit_action_run_lock_for_album(&album).unwrap();
        let error = ensure_album_publication_has_no_unresolved_explicit_state(&lock, &album)
            .expect_err("publishing must not replace the only manual recovery authority");
        assert!(matches!(error, ActionError::Conflict(_)));
    }

    #[test]
    fn unresolved_workspace_scan_fails_closed_on_malformed_action_authority() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let journals = workspace.join(".tonepoet-action-journals");
        fs::create_dir_all(&journals).unwrap();
        fs::write(journals.join("actions-bad.journal.json"), b"not json").unwrap();

        assert!(workspace_has_unresolved_action_state(&workspace));
    }

    #[test]
    fn unresolved_workspace_scan_fails_closed_on_unknown_reserved_authority() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let journals = workspace.join(".tonepoet-action-journals");
        fs::create_dir_all(&journals).unwrap();
        fs::write(journals.join("future-schema-authority.bin"), b"opaque").unwrap();

        assert!(
            workspace_has_unresolved_action_state(&workspace),
            "unknown entries in a reserved action authority directory must veto recursive cleanup"
        );
    }

    #[test]
    fn safely_terminal_action_journal_does_not_pin_parent_workspace_forever() {
        let fixture = Fixture::new();
        let pipeline = ActionPipeline {
            pre: Vec::new(),
            post: vec![ConversionAction::CreateFolder(CreateFolderAction {
                path: PathBuf::from("Created"),
                continue_on_error: false,
            })],
        };
        let context = fixture.context(ActionPhase::Post);
        let runner = RecordingRunner::default();
        engine(&runner)
            .execute_phase(&pipeline, &context, &NeverCancel)
            .unwrap();

        assert!(fixture.album_dir.join("Created").is_dir());
        assert!(
            !workspace_has_unresolved_action_state(
                context.journal_dir.parent().expect("journal parent")
            ),
            "cleanup-complete terminal journals are not unresolved recovery state"
        );
    }

    #[test]
    fn terminal_journal_retention_is_bounded_for_manual_and_non_batch_roots() {
        let fixture = Fixture::new();
        let pipeline = ActionPipeline {
            pre: Vec::new(),
            post: vec![ConversionAction::CreateFolder(CreateFolderAction {
                path: PathBuf::from("Retained"),
                continue_on_error: false,
            })],
        };
        let runner = RecordingRunner::default();
        for index in 0..(TERMINAL_JOURNAL_RETENTION_COUNT + 5) {
            let mut context = fixture.context(ActionPhase::Post);
            context.run_identity = format!("retention-run-{index:03}");
            engine(&runner)
                .execute_phase(&pipeline, &context, &NeverCancel)
                .unwrap();
        }

        let retained = fs::read_dir(&fixture.journal_dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".journal.json")
            })
            .count();
        assert!(retained <= TERMINAL_JOURNAL_RETENTION_COUNT);
    }

    #[test]
    fn explicit_manual_run_resumes_exact_unresolved_journal_and_rejects_changed_pipeline() {
        let fixture = Fixture::new();
        let mut context = fixture.context(ActionPhase::Post);
        context.explicit_scope = true;
        context.source_path = fixture.album_dir.clone();
        context.source_is_directory = true;
        context.subject_dir = fixture.album_dir.clone();
        context.protected_sources.clear();
        context.protected_generated_paths = [fixture.album_dir.join(".tonepoet-action-identity.json")]
            .into_iter()
            .collect();
        context.journal_dir = fixture.album_dir.join(".tonepoet-actions-manual");
        context.run_identity = "manual-published:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string();
        context.album_identity = "published:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string();

        let pipeline = ActionPipeline {
            pre: Vec::new(),
            post: vec![ConversionAction::CreateFolder(CreateFolderAction {
                path: PathBuf::from("Recovered"),
                continue_on_error: false,
            })],
        };
        let runner = RecordingRunner::default();
        let action_engine = engine(&runner);

        test_set_journal_persist_fault(Some(3));
        let first = action_engine.execute_explicit_phase(&pipeline, &context, &NeverCancel);
        test_set_journal_persist_fault(None);
        assert!(first.is_err(), "fault injection must leave durable non-terminal authority");
        assert_eq!(
            action_engine
                .inspect_explicit_recovery(&pipeline, &context)
                .expect("exact pipeline recovery inspection"),
            ExplicitActionRecoveryDisposition::Resume,
            "the next process-equivalent invocation must discover and resume the exact journal"
        );

        let changed = ActionPipeline {
            pre: Vec::new(),
            post: vec![ConversionAction::CreateFolder(CreateFolderAction {
                path: PathBuf::from("Different"),
                continue_on_error: false,
            })],
        };
        assert!(matches!(
            action_engine.inspect_explicit_recovery(&changed, &context),
            Err(ActionError::Conflict(_))
        ));

        let recovered = action_engine
            .execute_explicit_phase(&pipeline, &context, &NeverCancel)
            .expect("exact manual pipeline must resume");
        assert!(!recovered.recovery_required);
        assert!(fixture.album_dir.join("Recovered").is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn completed_explicit_runs_are_retired_and_each_new_command_executes_again() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = Fixture::new();
        let mut context = fixture.context(ActionPhase::Post);
        context.explicit_scope = true;
        context.source_path = fixture.album_dir.clone();
        context.source_is_directory = true;
        context.subject_dir = fixture.album_dir.clone();
        context.protected_sources.clear();
        context.protected_generated_paths = [fixture.album_dir.join(".tonepoet-action-identity.json")]
            .into_iter()
            .collect();
        context.journal_dir = fixture.album_dir.join(".tonepoet-actions-manual");
        context.run_identity = "manual-published:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string();
        context.album_identity = "published:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string();

        let script = fixture._temp.path().join("run-every-invocation.sh");
        fs::write(&script, b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let pipeline = ActionPipeline {
            pre: Vec::new(),
            post: vec![ConversionAction::Runscript(RunScriptAction {
                script,
                args: Vec::new(),
                timeout_seconds: 30,
                continue_on_error: false,
            })],
        };
        let runner = RecordingRunner::default();
        let action_engine = engine(&runner);

        action_engine
            .execute_explicit_phase(&pipeline, &context, &NeverCancel)
            .expect("first explicit invocation");
        assert_eq!(runner.calls.lock().unwrap().len(), 1);
        assert!(
            !explicit_active_run_path(&context).exists(),
            "a validated terminal invocation must retire its stable active-run pointer"
        );

        action_engine
            .execute_explicit_phase(&pipeline, &context, &NeverCancel)
            .expect("second explicit invocation");
        assert_eq!(
            runner.calls.lock().unwrap().len(),
            2,
            "a later :actions-run command must allocate a fresh invocation and execute again"
        );
        assert!(
            !explicit_active_run_path(&context).exists(),
            "the second terminal invocation must also retire its active-run pointer"
        );
    }

    #[test]
    fn completed_explicit_builtin_run_replans_current_tree_on_later_invocation() {
        let fixture = Fixture::new();
        let mut context = fixture.context(ActionPhase::Post);
        context.explicit_scope = true;
        context.source_path = fixture.album_dir.clone();
        context.source_is_directory = true;
        context.subject_dir = fixture.album_dir.clone();
        context.protected_sources.clear();
        context.protected_generated_paths = [fixture.album_dir.join(".tonepoet-action-identity.json")]
            .into_iter()
            .collect();
        context.journal_dir = fixture.album_dir.join(".tonepoet-actions-manual");
        context.run_identity = "manual-published:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc".to_string();
        context.album_identity = "published:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc".to_string();

        let pipeline = ActionPipeline {
            pre: Vec::new(),
            post: vec![ConversionAction::Delete(DeleteAction {
                targeting: targeting(&["*.pending"]),
            })],
        };
        let runner = RecordingRunner::default();
        let action_engine = engine(&runner);

        let first = fixture.album_dir.join("first.pending");
        fs::write(&first, b"first").unwrap();
        action_engine
            .execute_explicit_phase(&pipeline, &context, &NeverCancel)
            .expect("first explicit delete invocation");
        assert!(!first.exists());

        let second = fixture.album_dir.join("second.pending");
        fs::write(&second, b"second").unwrap();
        action_engine
            .execute_explicit_phase(&pipeline, &context, &NeverCancel)
            .expect("second explicit delete invocation");
        assert!(
            !second.exists(),
            "a later command must plan and apply against the current tree, not return an old report"
        );
    }

    #[test]
    fn terminal_active_run_from_old_pipeline_is_retired_before_new_pipeline_executes() {
        let fixture = Fixture::new();
        let mut base_context = fixture.context(ActionPhase::Post);
        base_context.explicit_scope = true;
        base_context.source_path = fixture.album_dir.clone();
        base_context.source_is_directory = true;
        base_context.subject_dir = fixture.album_dir.clone();
        base_context.protected_sources.clear();
        base_context.protected_generated_paths = [fixture.album_dir.join(".tonepoet-action-identity.json")]
            .into_iter()
            .collect();
        base_context.journal_dir = fixture.album_dir.join(".tonepoet-actions-manual");
        base_context.run_identity = "manual-published:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd".to_string();
        base_context.album_identity = "published:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd".to_string();

        let old_pipeline = ActionPipeline {
            pre: Vec::new(),
            post: vec![ConversionAction::CreateFolder(CreateFolderAction {
                path: PathBuf::from("old-terminal"),
                continue_on_error: false,
            })],
        };
        let new_pipeline = ActionPipeline {
            pre: Vec::new(),
            post: vec![ConversionAction::CreateFolder(CreateFolderAction {
                path: PathBuf::from("new-command"),
                continue_on_error: false,
            })],
        };
        let runner = RecordingRunner::default();
        let action_engine = engine(&runner);
        let mut old_context = base_context.clone();
        old_context.run_identity = format!("manual-invocation:{}", Uuid::new_v4());
        action_engine
            .execute_phase(&old_pipeline, &old_context, &NeverCancel)
            .expect("create terminal journal for old pipeline");
        let old_digest = old_pipeline.canonical_sha256().unwrap();
        let old_journal = action_journal_path(&old_context, &old_digest).unwrap();
        let old_temporary = journal_write_temporary_path(&old_journal).unwrap();
        fs::copy(&old_journal, &old_temporary).unwrap();
        let mut lock = acquire_explicit_action_run_lock(&base_context).unwrap();
        create_explicit_active_run_locked(
            &old_pipeline,
            &old_context,
            old_context.run_identity.clone(),
            None,
            &mut lock,
        )
        .unwrap();
        drop(lock);

        action_engine
            .execute_explicit_phase(&new_pipeline, &base_context, &NeverCancel)
            .expect("terminal old pipeline must retire before the new command");
        assert!(fixture.album_dir.join("old-terminal").is_dir());
        assert!(fixture.album_dir.join("new-command").is_dir());
        assert!(!explicit_active_run_path(&base_context).exists());
    }

    #[cfg(unix)]
    #[test]
    fn orphaned_terminal_journal_and_write_temporary_are_retired_without_replay() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = Fixture::new();
        let mut base_context = fixture.context(ActionPhase::Post);
        base_context.explicit_scope = true;
        base_context.source_path = fixture.album_dir.clone();
        base_context.source_is_directory = true;
        base_context.subject_dir = fixture.album_dir.clone();
        base_context.protected_sources.clear();
        base_context.protected_generated_paths = [fixture.album_dir.join(".tonepoet-action-identity.json")]
            .into_iter()
            .collect();
        base_context.journal_dir = fixture.album_dir.join(".tonepoet-actions-manual");
        base_context.run_identity = "manual-published:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee".to_string();
        base_context.album_identity = "published:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee".to_string();

        let script = fixture._temp.path().join("orphan-retirement.sh");
        fs::write(&script, b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let pipeline = ActionPipeline {
            pre: Vec::new(),
            post: vec![ConversionAction::Runscript(RunScriptAction {
                script,
                args: Vec::new(),
                timeout_seconds: 30,
                continue_on_error: false,
            })],
        };
        let runner = RecordingRunner::default();
        let action_engine = engine(&runner);
        let mut orphan_context = base_context.clone();
        orphan_context.run_identity = format!("manual-invocation:{}", Uuid::new_v4());
        action_engine
            .execute_phase(&pipeline, &orphan_context, &NeverCancel)
            .expect("create orphan terminal journal");
        let digest = pipeline.canonical_sha256().unwrap();
        let journal_path = action_journal_path(&orphan_context, &digest).unwrap();
        let temporary_path = journal_write_temporary_path(&journal_path).unwrap();
        let journal_bytes = fs::read(&journal_path).unwrap();
        fs::write(&temporary_path, &journal_bytes).unwrap();

        action_engine
            .execute_explicit_phase(&pipeline, &base_context, &NeverCancel)
            .expect("orphan terminal generations must retire before fresh execution");
        assert_eq!(
            runner.calls.lock().unwrap().len(),
            2,
            "the orphan terminal is history; the new invocation must still run exactly once"
        );
        assert!(!journal_path.exists());
        assert!(!temporary_path.exists());
    }

    #[test]
    fn sr2_flat_album_scope_fails_closed_without_touching_output_root() {
        let fixture = Fixture::new();
        let marker = fixture.output_root.join("shared.txt");
        fs::write(&marker, b"shared").unwrap();
        let mut context = fixture.context(ActionPhase::Post);
        context.album_dir = fixture.output_root.clone();
        context.subject_dir = fixture.output_root.clone();
        let runner = RecordingRunner::default();
        let action = ConversionAction::Delete(DeleteAction {
            targeting: targeting(&["*"]),
        });
        let error = engine(&runner).preview_action(0, &action, &context).unwrap_err();
        assert!(error.to_string().contains("proper subdirectory"));
        assert_eq!(fs::read(marker).unwrap(), b"shared");
    }

    #[test]
    fn rename_planner_invokes_production_sanitizer_and_retains_dot_runs() {
        let fixture = Fixture::new();
        let source = fixture.album_dir.join("plain.txt");
        fs::write(&source, b"plain").unwrap();
        let mut context = fixture.context(ActionPhase::Post);
        context.semantics = crate::convert::pipeline::stages::action_semantics();
        let action = ConversionAction::Rename(RenameAction {
            targeting: targeting(&["plain.txt"]),
            mode: RenameMode::Template,
            template: "...%STEM%...".to_string(),
        });
        let runner = RecordingRunner::default();
        let plan = engine(&runner)
            .preview_action(0, &action, &context)
            .expect("template rename should plan");

        let destination = plan.operations.iter().find_map(|operation| match operation {
            PlannedOperation::Rename { destination, .. } => Some(destination),
            _ => None,
        }).expect("rename operation");
        assert_eq!(
            destination.file_name().and_then(|name| name.to_str()),
            Some("...plain....txt")
        );
    }

    #[test]
    fn sr3_protected_source_is_refused_per_path_while_other_matches_remain_planned() {
        let fixture = Fixture::new();
        let protected = fixture.album_dir.join("source.flac");
        let ordinary = fixture.album_dir.join("notes.txt");
        fs::write(&protected, b"master").unwrap();
        fs::write(&ordinary, b"notes").unwrap();
        let mut context = fixture.context(ActionPhase::Post);
        context.protected_sources = [protected.clone()].into_iter().collect();
        let action = ConversionAction::Delete(DeleteAction {
            targeting: targeting(&["*"]),
        });
        let runner = RecordingRunner::default();
        let plan = engine(&runner).preview_action(0, &action, &context).unwrap();
        assert_eq!(operation_targets(&plan), vec![ordinary]);
        assert!(plan.notices.iter().any(|notice| notice.contains("source protection refused")));
    }

    #[test]
    fn sr4_pre_phase_restricts_mutating_builtins() {
        let fixture = Fixture::new();
        let runner = RecordingRunner::default();
        let action = ConversionAction::Delete(DeleteAction {
            targeting: targeting(&["*"]),
        });
        let error = engine(&runner)
            .preview_action(0, &action, &fixture.context(ActionPhase::Pre))
            .unwrap_err();
        assert!(error.to_string().contains("pre phase permits only"));
    }

    #[test]
    fn sr5_rename_end_state_supports_cycles_without_last_write_wins() {
        let fixture = Fixture::new();
        fs::write(fixture.album_dir.join("a.txt"), b"A").unwrap();
        fs::write(fixture.album_dir.join("b.txt"), b"B").unwrap();
        let pipeline = ActionPipeline {
            pre: Vec::new(),
            post: vec![ConversionAction::Rename(RenameAction {
                targeting: targeting(&["a.txt", "b.txt"]),
                mode: RenameMode::Template,
                template: "%STEM%".to_string(),
            })],
        };
        // The identity template is a no-op. Exercise the shared planner's
        // actual cycle map directly because one template cannot express an
        // arbitrary pairwise swap.
        let transaction = plan_rename_transaction(
            &fixture.album_dir,
            [
                RenameIntent {
                    source: fixture.album_dir.join("a.txt"),
                    destination: fixture.album_dir.join("b.txt"),
                },
                RenameIntent {
                    source: fixture.album_dir.join("b.txt"),
                    destination: fixture.album_dir.join("a.txt"),
                },
            ],
        )
        .unwrap();
        assert_eq!(transaction.entries.len(), 2);
        assert_eq!(transaction.staging_order().len(), 2);
        assert_eq!(pipeline.post.len(), 1);
    }

    #[test]
    fn sr5_collision_is_rejected_against_planned_end_state() {
        let fixture = Fixture::new();
        fs::write(fixture.album_dir.join("a.log"), b"A").unwrap();
        fs::write(fixture.album_dir.join("b.log"), b"B").unwrap();
        let action = ConversionAction::Rename(RenameAction {
            targeting: targeting(&["*.log"]),
            mode: RenameMode::Template,
            template: "same".to_string(),
        });
        let runner = RecordingRunner::default();
        let error = engine(&runner)
            .preview_action(0, &action, &fixture.context(ActionPhase::Post))
            .unwrap_err();
        assert!(
            error.to_string().contains("end-state collision"),
            "SR-5 requires a planned end-state collision refusal, got: {error}"
        );
    }

    #[test]
    fn action_destination_collision_guard_remains_conservative_for_case_variants() {
        let mut planned = BTreeMap::new();
        let upper = PathBuf::from("/out/Album/track.flac");
        let lower = PathBuf::from("/out/album/track.flac");

        register_planned_destination(&mut planned, &upper, "copy")
            .expect("first destination registers");
        let error = register_planned_destination(&mut planned, &lower, "copy")
            .expect_err("case-only destination variants remain a conservative action collision");
        assert!(matches!(&error, ActionError::Conflict(_)));
        assert!(error.to_string().contains("end-state collision"));
    }

    #[test]
    fn sr5_copy_rejects_two_sources_with_the_same_planned_destination() {
        let fixture = Fixture::new();
        fs::create_dir_all(fixture.album_dir.join("disc 01")).unwrap();
        fs::create_dir_all(fixture.album_dir.join("disc 02")).unwrap();
        fs::write(fixture.album_dir.join("disc 01").join("rip.log"), b"one").unwrap();
        fs::write(fixture.album_dir.join("disc 02").join("rip.log"), b"two").unwrap();
        let action = ConversionAction::Copy(CopyAction {
            targeting: targeting(&["*.log"]),
            destination: PathBuf::from("collected"),
        });
        let runner = RecordingRunner::default();
        let error = engine(&runner)
            .preview_action(0, &action, &fixture.context(ActionPhase::Post))
            .unwrap_err();
        assert!(error.to_string().contains("end-state collision"));
    }

    #[test]
    fn live_supervisor_failure_triggers_immediate_containment_recovery() {
        let fixture = Fixture::new();
        let script = fixture._temp.path().join("failed-supervisor-script.sh");
        fs::write(&script, b"#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let pipeline = ActionPipeline {
            pre: Vec::new(),
            post: vec![ConversionAction::Runscript(RunScriptAction {
                script,
                args: Vec::new(),
                timeout_seconds: 30,
                continue_on_error: false,
            })],
        };
        let context = fixture.context(ActionPhase::Post);
        let filesystem = CapabilityActionFilesystem::new();
        let runner = FailAfterPreparedRunner::default();
        let engine = ActionEngine {
            filesystem: &filesystem,
            scripts: &runner,
        };
        let error = engine
            .execute_phase(&pipeline, &context, &NeverCancel)
            .expect_err("post-start supervisor failure must remain manual recovery");
        assert!(matches!(error, ActionError::ManualRecoveryRequired(_)));
        assert_eq!(*runner.recover_calls.lock().unwrap(), 1);
    }

    #[test]
    fn durably_started_script_is_recovered_and_never_replayed() {
        let fixture = Fixture::new();
        let script = fixture._temp.path().join("crash-script.sh");
        fs::write(&script, b"#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let pipeline = ActionPipeline {
            pre: Vec::new(),
            post: vec![ConversionAction::Runscript(RunScriptAction {
                script,
                args: Vec::new(),
                timeout_seconds: 30,
                continue_on_error: false,
            })],
        };
        let context = fixture.context(ActionPhase::Post);
        let filesystem = CapabilityActionFilesystem::new();
        let crashing_engine = ActionEngine {
            filesystem: &filesystem,
            scripts: &PanicAfterPreparedRunner,
        };
        let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = crashing_engine.execute_phase(&pipeline, &context, &NeverCancel);
        }));
        assert!(crashed.is_err(), "crash injection did not trigger");

        let recovery = RecoveryOnlyRunner::default();
        let recovering_engine = ActionEngine {
            filesystem: &filesystem,
            scripts: &recovery,
        };
        let error = recovering_engine
            .execute_phase(&pipeline, &context, &NeverCancel)
            .expect_err("interrupted script must require manual recovery");
        assert!(matches!(error, ActionError::ManualRecoveryRequired(_)));
        assert_eq!(*recovery.run_calls.lock().unwrap(), 0);
        assert_eq!(*recovery.recover_calls.lock().unwrap(), 1);
    }

    #[test]
    fn recovered_exec_gate_never_released_is_terminal_setup_failure_not_cancellation() {
        let fixture = Fixture::new();
        let script = fixture._temp.path().join("never-released-script.sh");
        fs::write(&script, b"#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let pipeline = ActionPipeline {
            pre: Vec::new(),
            post: vec![ConversionAction::Runscript(RunScriptAction {
                script,
                args: Vec::new(),
                timeout_seconds: 30,
                continue_on_error: false,
            })],
        };
        let context = fixture.context(ActionPhase::Post);
        let filesystem = CapabilityActionFilesystem::new();
        let crashing_engine = ActionEngine {
            filesystem: &filesystem,
            scripts: &PanicAfterPreparedRunner,
        };
        let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = crashing_engine.execute_phase(&pipeline, &context, &NeverCancel);
        }));
        assert!(crashed.is_err(), "crash injection did not trigger");

        let recovery = NeverReleasedRecoveryRunner::default();
        let recovering_engine = ActionEngine {
            filesystem: &filesystem,
            scripts: &recovery,
        };
        let report = recovering_engine
            .execute_phase(&pipeline, &context, &NeverCancel)
            .expect("verified never-released execution must terminalize deterministically");
        assert_eq!(*recovery.run_calls.lock().unwrap(), 0);
        assert_eq!(*recovery.recover_calls.lock().unwrap(), 1);
        assert_eq!(report.actions.len(), 1);
        assert_eq!(report.actions[0].status, ActionResultStatus::Failed);
        assert!(!report.recovery_required);
        assert!(report.actions[0]
            .operations
            .iter()
            .all(|operation| operation.status == OperationResultStatus::Failed));
    }

    #[test]
    fn sr6_preview_never_invokes_script_runner() {
        let fixture = Fixture::new();
        let script = fixture._temp.path().join("script.sh");
        fs::write(&script, b"#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let runner = RecordingRunner::default();
        let action = ConversionAction::Runscript(RunScriptAction {
            script,
            args: vec!["literal arg".to_string()],
            timeout_seconds: 30,
            continue_on_error: false,
        });
        let plan = engine(&runner)
            .preview_action(0, &action, &fixture.context(ActionPhase::Post))
            .unwrap();
        assert_eq!(runner.calls.lock().unwrap().len(), 0);
        assert!(describe_plan(&plan)[0].starts_with("would run:"));
    }

    #[test]
    fn script_environment_is_sanitized_and_preserves_apostrophes() {
        let fixture = Fixture::new();
        let script = fixture._temp.path().join("script.sh");
        fs::write(&script, b"#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let mut context = fixture.context(ActionPhase::Post);
        context
            .album_tokens
            .insert("ALBUM".to_string(), "Nobody's\nPerfect\u{0007}".to_string());
        let pipeline = ActionPipeline {
            pre: Vec::new(),
            post: vec![ConversionAction::Runscript(RunScriptAction {
                script,
                args: vec!["literal;not-shell".to_string()],
                timeout_seconds: 30,
                continue_on_error: false,
            })],
        };
        let runner = RecordingRunner::default();
        let report = engine(&runner)
            .execute_phase(&pipeline, &context, &NeverCancel)
            .unwrap();
        assert!(!report.has_errors());
        assert!(report.actions[0]
            .notices
            .iter()
            .any(|notice| notice.contains("process_tree_observed")));
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].args, vec!["literal;not-shell"]);
        assert_eq!(calls[0].environment["TONEPOET_ALBUM"], "Nobody'sPerfect");
    }

    #[cfg(unix)]
    #[test]
    fn retained_capabilities_precede_lexical_validation_and_journal_reads() {
        let temp = tempfile::tempdir().expect("temp dir");
        let logical_output = temp.path().join("output");
        let logical_album = logical_output.join("Artist").join("Album");
        let logical_journal = logical_output.join(".coord").join("journals");
        let source = temp.path().join("source.flac");
        fs::create_dir_all(&logical_album).expect("album");
        fs::create_dir_all(&logical_journal).expect("journal");
        fs::write(&source, b"source").expect("source");

        let retained_output = Arc::new(
            PinnedDirectoryCapability::open_trusted(&logical_output)
                .expect("retain output"),
        );
        let retained_album = Arc::new(
            PinnedDirectoryCapability::open_trusted(&logical_album)
                .expect("retain album"),
        );
        let retained_journal = Arc::new(
            PinnedDirectoryCapability::open_trusted(&logical_journal)
                .expect("retain journal"),
        );
        let context = ActionContext {
            run_identity: "retained-run".to_string(),
            album_identity: "retained-album".to_string(),
            phase: ActionPhase::Post,
            subject_dir: logical_album.clone(),
            source_path: source.clone(),
            source_is_directory: false,
            output_root: logical_output.clone(),
            album_dir: logical_album.clone(),
            environment_album_dir: Some(logical_album.clone()),
            retained_album_capability: Some(retained_album),
            retained_output_capability: Some(retained_output),
            retained_journal_capability: Some(retained_journal),
            coordination_io_dir: None,
            protected_sources: [source].into_iter().collect(),
            protected_generated_paths: BTreeSet::new(),
            album_tokens: BTreeMap::new(),
            disc_count: None,
            journal_dir: logical_journal.clone(),
            batch_source_scope_root: None,
            explicit_scope: false,
            semantics: test_semantics(),
        };
        let pipeline = ActionPipeline {
            pre: Vec::new(),
            post: vec![ConversionAction::CreateFolder(CreateFolderAction {
                path: PathBuf::from("Created"),
                continue_on_error: false,
            })],
        };

        let retained_output_path = temp.path().join("retained-output");
        fs::rename(&logical_output, &retained_output_path).expect("replace output root");
        fs::create_dir_all(&logical_output).expect("replacement output root");

        let filesystem = CapabilityActionFilesystem::new();
        let runner = RecordingRunner::default();
        let engine = ActionEngine {
            filesystem: &filesystem,
            scripts: &runner,
        };
        let report = engine
            .execute_phase(&pipeline, &context, &NeverCancel)
            .expect("execute through retained capabilities");
        assert!(!report.has_errors());
        assert!(
            retained_output_path
                .join("Artist")
                .join("Album")
                .join("Created")
                .is_dir(),
            "mutation must follow the retained album object"
        );
        assert!(
            !logical_output.join("Artist").join("Album").exists(),
            "replacement lexical output must remain untouched"
        );

        let report_filesystem = CapabilityActionFilesystem::new();
        let report_engine = ActionEngine {
            filesystem: &report_filesystem,
            scripts: &runner,
        };
        let durable = report_engine
            .durable_phase_report(&pipeline, &context)
            .expect("read report through retained journal capability")
            .expect("durable report");
        assert_eq!(durable.actions, report.actions);
        assert!(
            retained_output_path
                .join(".coord")
                .join("journals")
                .read_dir()
                .expect("retained journal directory")
                .next()
                .is_some(),
            "journal must be written beneath the retained output object"
        );
    }

    #[test]
    fn exec_gated_script_cancellation_is_terminal_before_mutation() {
        let fixture = Fixture::new();
        let script = fixture._temp.path().join("script.sh");
        fs::write(&script, b"#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let pipeline = ActionPipeline {
            pre: Vec::new(),
            post: vec![ConversionAction::Runscript(RunScriptAction {
                script,
                args: Vec::new(),
                timeout_seconds: 30,
                continue_on_error: false,
            })],
        };
        let context = fixture.context(ActionPhase::Post);
        let filesystem = CapabilityActionFilesystem::new();
        let engine = ActionEngine {
            filesystem: &filesystem,
            scripts: &ExecGatedCancellationRunner,
        };
        let result = engine.execute_phase(&pipeline, &context, &NeverCancel);
        assert!(matches!(result, Err(ActionError::CancelledBeforeMutation(_))));
        let report = engine
            .durable_phase_report(&pipeline, &context)
            .unwrap()
            .unwrap();
        assert!(report.cancelled);
        assert!(!report.recovery_required);
        assert_eq!(
            report.actions[0].status,
            ActionResultStatus::CancelledBeforeMutation
        );
    }

    #[test]
    fn builtins_are_idempotent_after_terminal_success() {
        let fixture = Fixture::new();
        let pipeline = ActionPipeline {
            pre: Vec::new(),
            post: vec![ConversionAction::CreateFolder(CreateFolderAction {
                path: PathBuf::from("Scans"),
                continue_on_error: false,
            })],
        };
        let runner = RecordingRunner::default();
        let context = fixture.context(ActionPhase::Post);
        let first = engine(&runner)
            .execute_phase(&pipeline, &context, &NeverCancel)
            .unwrap();
        let second = engine(&runner)
            .execute_phase(&pipeline, &context, &NeverCancel)
            .unwrap();
        assert_eq!(first, second);
        assert!(fixture.album_dir.join("Scans").is_dir());
    }

    #[test]
    fn copy_recovers_after_every_injected_journal_transition() {
        for fail_at in 1..=20 {
            let fixture = Fixture::new();
            fs::write(fixture.album_dir.join("booklet.txt"), b"booklet").unwrap();
            let pipeline = ActionPipeline {
                pre: Vec::new(),
                post: vec![ConversionAction::Copy(CopyAction {
                    targeting: targeting(&["booklet.txt"]),
                    destination: PathBuf::from("Copies"),
                })],
            };
            let context = fixture.context(ActionPhase::Post);
            let runner = RecordingRunner::default();
            test_set_journal_persist_fault(Some(fail_at));
            let _ = engine(&runner).execute_phase(&pipeline, &context, &NeverCancel);
            test_set_journal_persist_fault(None);
            let report = engine(&runner)
                .execute_phase(&pipeline, &context, &NeverCancel)
                .unwrap_or_else(|error| panic!("recovery after persist call {fail_at}: {error}"));
            assert!(!report.recovery_required, "persist call {fail_at}");
            assert_eq!(
                fs::read(fixture.album_dir.join("Copies").join("booklet.txt")).unwrap(),
                b"booklet",
                "persist call {fail_at}"
            );
        }
    }

    #[test]
    fn move_and_delete_recover_without_deleting_recreated_paths() {
        let fixture = Fixture::new();
        let source = fixture.album_dir.join("move.txt");
        fs::write(&source, b"original").unwrap();
        let pipeline = ActionPipeline {
            pre: Vec::new(),
            post: vec![ConversionAction::Move(MoveAction {
                targeting: targeting(&["move.txt"]),
                destination: PathBuf::from("Moved"),
            })],
        };
        let context = fixture.context(ActionPhase::Post);
        let runner = RecordingRunner::default();
        test_set_journal_persist_fault(Some(7));
        let _ = engine(&runner).execute_phase(&pipeline, &context, &NeverCancel);
        test_set_journal_persist_fault(None);
        if !source.exists() {
            fs::write(&source, b"replacement").unwrap();
        }
        let recovery = engine(&runner).execute_phase(&pipeline, &context, &NeverCancel);
        if source.exists() && fs::read(&source).unwrap() == b"replacement" {
            assert!(recovery.is_err(), "source recreation must be a contradiction");
            assert_eq!(fs::read(&source).unwrap(), b"replacement");
        }
    }

    #[test]
    fn tampered_journal_destination_fails_closed() {
        let fixture = Fixture::new();
        fs::write(fixture.album_dir.join("a.txt"), b"A").unwrap();
        let pipeline = ActionPipeline {
            pre: Vec::new(),
            post: vec![ConversionAction::Copy(CopyAction {
                targeting: targeting(&["a.txt"]),
                destination: PathBuf::from("Copies"),
            })],
        };
        let context = fixture.context(ActionPhase::Post);
        let runner = RecordingRunner::default();
        test_set_journal_persist_fault(Some(3));
        let _ = engine(&runner).execute_phase(&pipeline, &context, &NeverCancel);
        test_set_journal_persist_fault(None);
        let serialized = pipeline.canonical_serialization().unwrap();
        let journal_path = action_journal_path(&context, &sha256_hex(serialized.as_bytes())).unwrap();
        let mut json: serde_json::Value = serde_json::from_slice(&fs::read(&journal_path).unwrap()).unwrap();
        json["actions"][0]["operations"][0]["plan"]["destination"] =
            serde_json::Value::String(fixture._temp.path().join("escape.txt").display().to_string());
        fs::write(&journal_path, serde_json::to_vec_pretty(&json).unwrap()).unwrap();
        let error = engine(&runner)
            .execute_phase(&pipeline, &context, &NeverCancel)
            .unwrap_err();
        assert!(matches!(error, ActionError::InvalidJournal(_) | ActionError::UnsafePath(_)));
        assert!(!fixture._temp.path().join("escape.txt").exists());
    }

    #[test]
    fn cancellation_before_mutation_is_terminal_and_does_not_run_on_recovery() {
        struct AlwaysCancelled;
        impl ActionCancellation for AlwaysCancelled {
            fn is_cancelled(&self) -> bool { true }
        }
        let fixture = Fixture::new();
        let pipeline = ActionPipeline {
            pre: Vec::new(),
            post: vec![ConversionAction::CreateFolder(CreateFolderAction {
                path: PathBuf::from("Never"),
                continue_on_error: false,
            })],
        };
        let context = fixture.context(ActionPhase::Post);
        let runner = RecordingRunner::default();
        let first = engine(&runner).execute_phase(&pipeline, &context, &AlwaysCancelled);
        assert!(matches!(first, Err(ActionError::CancelledBeforeMutation(_))));
        let recovered = engine(&runner)
            .execute_phase(&pipeline, &context, &NeverCancel)
            .unwrap();
        assert!(recovered.cancelled);
        assert!(!fixture.album_dir.join("Never").exists());
    }

    fn single_noop_pipeline() -> ActionPipeline {
        ActionPipeline {
            pre: Vec::new(),
            post: vec![ConversionAction::CreateFolder(CreateFolderAction {
                path: PathBuf::from("AlreadyThere"),
                continue_on_error: false,
            })],
        }
    }

    fn single_noop_report() -> ActionPhaseReport {
        ActionPhaseReport {
            phase: Some(ActionPhase::Post),
            actions: vec![ActionResult {
                index: 0,
                kind: "create_folder".to_string(),
                status: ActionResultStatus::NoOp,
                operations: Vec::new(),
                error: None,
                notices: Vec::new(),
            }],
            notices: Vec::new(),
            recovery_required: false,
            cancelled: false,
        }
    }

    #[test]
    fn election_result_is_bound_to_every_configured_action_slot() {
        let pipeline = single_noop_pipeline();
        let election = ActionElectionIdentity::new(
            "run",
            "album",
            ActionPhase::Post,
            &pipeline,
        )
        .unwrap();
        let mut report = single_noop_report();
        validate_election_report(&report, &election).unwrap();

        report.actions.clear();
        assert!(matches!(
            validate_election_report(&report, &election),
            Err(ActionError::Election(_))
        ));

        let mut report = single_noop_report();
        report.actions[0].kind = "delete".to_string();
        assert!(matches!(
            validate_election_report(&report, &election),
            Err(ActionError::Election(_))
        ));
    }

    #[test]
    fn runner_refuses_to_publish_a_mismatched_terminal_report() {
        let temp = tempfile::tempdir().unwrap();
        let pipeline = single_noop_pipeline();
        let election = ActionElectionIdentity::new(
            "run",
            "album",
            ActionPhase::Post,
            &pipeline,
        )
        .unwrap();
        let guard = match elect_action_runner(temp.path(), &election, true).unwrap() {
            ActionElection::Runner(guard) => guard,
            other => panic!("expected runner election, got {other:?}"),
        };
        let claim_path = temp
            .path()
            .join(format!("{}.claim.json", election_file_stem(&election)));
        let result_path = temp
            .path()
            .join(format!("{}.result.json", election_file_stem(&election)));
        let invalid = ActionPhaseReport {
            phase: Some(ActionPhase::Post),
            ..ActionPhaseReport::default()
        };
        assert!(matches!(guard.finish(&invalid), Err(ActionError::Election(_))));
        assert!(claim_path.exists());
        assert!(!result_path.exists());
    }

    #[test]
    fn terminal_result_remains_bound_to_the_exact_removed_claim() {
        let temp = tempfile::tempdir().unwrap();
        let pipeline = single_noop_pipeline();
        let election = ActionElectionIdentity::new(
            "run",
            "album",
            ActionPhase::Post,
            &pipeline,
        )
        .unwrap();
        let guard = match elect_action_runner(temp.path(), &election, true).unwrap() {
            ActionElection::Runner(guard) => guard,
            other => panic!("expected runner election, got {other:?}"),
        };
        guard.finish(&single_noop_report()).unwrap();

        let report = match elect_action_runner(temp.path(), &election, true).unwrap() {
            ActionElection::Complete(report) => report,
            other => panic!("expected completed election, got {other:?}"),
        };
        assert_eq!(report, single_noop_report());

        let result_path = temp
            .path()
            .join(format!("{}.result.json", election_file_stem(&election)));
        let mut record: ActionResultRecord =
            serde_json::from_slice(&fs::read(&result_path).unwrap()).unwrap();
        record.claim.created_unix_nanos = record.claim.created_unix_nanos.saturating_add(1);
        fs::write(&result_path, serde_json::to_vec_pretty(&record).unwrap()).unwrap();
        assert!(matches!(
            elect_action_runner(temp.path(), &election, true),
            Err(ActionError::Election(message)) if message.contains("exact claim binding")
        ));
    }

    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_vendor = "apple",
        target_os = "redox"
    ))]
    #[test]
    fn atomic_no_clobber_rename_preserves_both_objects_on_conflict() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        fs::write(&source, b"source").unwrap();
        fs::write(&destination, b"destination").unwrap();

        assert!(matches!(
            rename_path_no_clobber(&source, &destination),
            Err(ActionError::Conflict(_))
        ));
        assert_eq!(fs::read(&source).unwrap(), b"source");
        assert_eq!(fs::read(&destination).unwrap(), b"destination");
    }

    #[test]
    fn current_process_mutation_authority_requires_registered_claim_uuid() {
        let unregistered = process_owner_identity(false);
        assert!(
            !owner_is_current_process(&unregistered).expect("valid owner identity"),
            "matching PID/start identity alone must not authorize mutation for an unregistered claim UUID"
        );

        let registered = current_process_owner().expect("registered owner");
        assert!(
            owner_is_current_process(&registered).expect("registered owner identity"),
            "the exact live registered claim must authorize its workspace"
        );
        release_current_process_owner_claim(&registered.claim_id);
        assert!(
            !owner_is_current_process(&registered).expect("released owner identity"),
            "releasing a claim must revoke current-process mutation authority"
        );
    }

    #[test]
    fn remote_owner_claim_is_never_declared_stale_from_a_local_pid_lookup() {
        let temp = tempfile::tempdir().unwrap();
        let pipeline = single_noop_pipeline();
        let election = ActionElectionIdentity::new(
            "run",
            "album",
            ActionPhase::Post,
            &pipeline,
        )
        .unwrap();
        let mut owner = current_process_owner().unwrap();
        owner.machine_identity = format!("foreign-{}", owner.machine_identity);
        owner.claim_id = Uuid::new_v4().to_string();
        let record = ActionClaimRecord {
            schema_version: CLAIM_SCHEMA_VERSION,
            election: election.clone(),
            owner,
            created_unix_nanos: now_unix_nanos(),
        };
        let claim_path = temp
            .path()
            .join(format!("{}.claim.json", election_file_stem(&election)));
        fs::create_dir_all(temp.path()).unwrap();
        write_json_create_new_durable(&claim_path, &record).unwrap();
        assert!(matches!(
            elect_action_runner(temp.path(), &election, true),
            Err(ActionError::Election(message)) if message.contains("remote/unknown")
        ));
        assert!(claim_path.exists());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn hostname_change_does_not_wedge_same_machine_same_boot_recovery() {
        let mut owner = current_process_owner().unwrap();
        owner.host_identity = format!("renamed-{}", owner.host_identity);
        owner.claim_id = Uuid::new_v4().to_string();
        assert_eq!(owner_liveness(&owner).unwrap(), OwnerLiveness::Alive);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn cloned_machine_identity_with_foreign_host_and_boot_fails_closed() {
        let mut owner = current_process_owner().unwrap();
        owner.host_identity = format!("foreign-{}", owner.host_identity);
        owner.boot_identity = format!("foreign-{}", owner.boot_identity);
        owner.claim_id = Uuid::new_v4().to_string();
        assert_eq!(
            owner_liveness(&owner).unwrap(),
            OwnerLiveness::RemoteOrUnknown
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn weak_process_start_evidence_never_authorizes_dead_owner_takeover() {
        let mut owner = current_process_owner().unwrap();
        owner.process_start_identity = format!("process-start-unavailable-{}", owner.pid);
        owner.claim_id = Uuid::new_v4().to_string();
        assert_eq!(
            owner_liveness(&owner).unwrap(),
            OwnerLiveness::RemoteOrUnknown
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn stable_process_start_evidence_allows_only_proven_dead_local_takeover() {
        let temp = tempfile::tempdir().unwrap();
        let pipeline = single_noop_pipeline();
        let election = ActionElectionIdentity::new(
            "run",
            "album",
            ActionPhase::Post,
            &pipeline,
        )
        .unwrap();
        let mut owner = current_process_owner().unwrap();
        owner.host_identity = "host-unavailable".to_string();
        owner.process_start_identity = format!("{}-reused", owner.process_start_identity);
        owner.claim_id = Uuid::new_v4().to_string();
        let old_claim_id = owner.claim_id.clone();
        let record = ActionClaimRecord {
            schema_version: CLAIM_SCHEMA_VERSION,
            election: election.clone(),
            owner,
            created_unix_nanos: now_unix_nanos(),
        };
        let stem = election_file_stem(&election);
        let claim_path = temp.path().join(format!("{stem}.claim.json"));
        write_json_create_new_durable(&claim_path, &record).unwrap();

        assert!(matches!(
            elect_action_runner(temp.path(), &election, false),
            Err(ActionError::Election(_))
        ));
        let guard = match elect_action_runner(temp.path(), &election, true).unwrap() {
            ActionElection::Runner(guard) => guard,
            other => panic!("expected takeover runner, got {other:?}"),
        };
        assert!(temp
            .path()
            .join(format!("{stem}.stale-{old_claim_id}.claim.json"))
            .exists());
        drop(guard);
    }

    #[test]
    fn cancelled_waiter_does_not_change_another_runners_claim() {
        struct AlwaysCancelled;
        impl ActionCancellation for AlwaysCancelled {
            fn is_cancelled(&self) -> bool {
                true
            }
        }

        let temp = tempfile::tempdir().unwrap();
        let pipeline = single_noop_pipeline();
        let election = ActionElectionIdentity::new(
            "run",
            "album",
            ActionPhase::Post,
            &pipeline,
        )
        .unwrap();
        let guard = match elect_action_runner(temp.path(), &election, true).unwrap() {
            ActionElection::Runner(guard) => guard,
            other => panic!("expected runner, got {other:?}"),
        };
        let claim_path = temp
            .path()
            .join(format!("{}.claim.json", election_file_stem(&election)));
        let claim_before = fs::read(&claim_path).unwrap();
        let waiter = match elect_action_runner(temp.path(), &election, true).unwrap() {
            ActionElection::Wait(waiter) => waiter,
            other => panic!("expected waiter, got {other:?}"),
        };
        assert!(matches!(
            waiter.wait(&AlwaysCancelled, Duration::from_millis(1)),
            Err(ActionError::CancelledBeforeMutation(_))
        ));
        assert_eq!(fs::read(&claim_path).unwrap(), claim_before);
        drop(guard);
    }

    #[test]
    fn missing_external_destination_root_is_durably_materialized_and_generation_bound() {
        let fixture = Fixture::new();
        let source = fixture.album_dir.join("booklet.txt");
        fs::write(&source, b"booklet").unwrap();
        let external_root = fixture._temp.path().join("external").join("album-copy");
        let pipeline = ActionPipeline {
            pre: Vec::new(),
            post: vec![ConversionAction::Copy(CopyAction {
                targeting: targeting(&["booklet.txt"]),
                destination: external_root.clone(),
            })],
        };
        let context = fixture.context(ActionPhase::Post);
        let runner = RecordingRunner::default();
        let filesystem = CapabilityActionFilesystem::new();
        let engine = ActionEngine {
            filesystem: &filesystem,
            scripts: &runner,
        };
        engine.execute_phase(&pipeline, &context, &NeverCancel).unwrap();
        assert_eq!(fs::read(external_root.join("booklet.txt")).unwrap(), b"booklet");

        let serialized = pipeline.canonical_serialization().unwrap();
        let journal_path =
            action_journal_path(&context, &sha256_hex(serialized.as_bytes())).unwrap();
        let newer = deserialize_action_journal(&fs::read(journal_path).unwrap()).unwrap();
        assert_eq!(
            newer.actions[0].root_materialization,
            RootMaterializationState::Complete,
            "destination-root mutation must be durably represented in the action journal"
        );
        // V8 shared-materialization design: the durable scope is the FIRST
        // missing component below the nearest existing ancestor (here
        // `external`), shared by all sibling destinations — not the full
        // destination path (see CORRECTED_BUNDLE_NOTES_V8).
        let external_boundary = fixture._temp.path().join("external");
        let external_index = newer
            .capability_roots
            .iter()
            .position(|record| record.logical_path == external_boundary)
            .unwrap_or_else(|| panic!(
                "external destination boundary scope should be journaled; roots: {:?}",
                newer.capability_roots.iter().map(|r| r.logical_path.clone()).collect::<Vec<_>>()
            ));
        assert!(newer.capability_roots[external_index].materialized_device.is_some());
        assert!(newer.capability_roots[external_index].materialized_inode.is_some());

        let mut older = newer.clone();
        older.generation = older.generation.saturating_sub(1);
        older.capability_roots[external_index].materialized_device = None;
        older.capability_roots[external_index].materialized_inode = None;
        validate_owned_journal_generation(&newer, &older).unwrap();

        let mut contradictory = newer.clone();
        contradictory.generation += 1;
        contradictory.capability_roots[external_index].materialized_inode = contradictory
            .capability_roots[external_index]
            .materialized_inode
            .and_then(|inode| inode.checked_add(1));
        assert!(matches!(
            validate_owned_journal_generation(&contradictory, &newer),
            Err(ActionError::Contradiction(_))
        ));
    }

    #[test]
    fn repeated_actions_share_one_missing_external_destination_capability() {
        let fixture = Fixture::new();
        fs::write(fixture.album_dir.join("one.txt"), b"one").unwrap();
        fs::write(fixture.album_dir.join("two.txt"), b"two").unwrap();
        let external_root = fixture._temp.path().join("external-shared").join("album");
        let pipeline = ActionPipeline {
            pre: Vec::new(),
            post: vec![
                ConversionAction::Copy(CopyAction {
                    targeting: targeting(&["one.txt"]),
                    destination: external_root.clone(),
                }),
                ConversionAction::Copy(CopyAction {
                    targeting: targeting(&["two.txt"]),
                    destination: external_root.clone(),
                }),
            ],
        };
        let context = fixture.context(ActionPhase::Post);
        let runner = RecordingRunner::default();
        let filesystem = CapabilityActionFilesystem::new();
        let engine = ActionEngine {
            filesystem: &filesystem,
            scripts: &runner,
        };
        engine.execute_phase(&pipeline, &context, &NeverCancel).unwrap();
        assert_eq!(fs::read(external_root.join("one.txt")).unwrap(), b"one");
        assert_eq!(fs::read(external_root.join("two.txt")).unwrap(), b"two");
        let records = filesystem.scope_records().unwrap();
        // V8 shared-materialization: both copies share ONE durable authority
        // at the first missing component (`external-shared`), and no
        // independent child-root records exist for the full destination.
        let boundary = fixture._temp.path().join("external-shared");
        assert_eq!(
            records
                .iter()
                .filter(|record| record.logical_path == boundary)
                .count(),
            1
        );
        assert_eq!(
            records
                .iter()
                .filter(|record| record.logical_path == external_root)
                .count(),
            0
        );
    }

    #[test]
    fn sibling_external_destinations_share_their_first_missing_parent_authority() {
        let fixture = Fixture::new();
        fs::write(fixture.album_dir.join("one.log"), b"log").unwrap();
        fs::write(fixture.album_dir.join("two.cue"), b"cue").unwrap();
        let external_parent = fixture._temp.path().join("external-exports");
        let pipeline = ActionPipeline {
            pre: Vec::new(),
            post: vec![
                ConversionAction::Copy(CopyAction {
                    targeting: targeting(&["one.log"]),
                    destination: external_parent.join("logs"),
                }),
                ConversionAction::Copy(CopyAction {
                    targeting: targeting(&["two.cue"]),
                    destination: external_parent.join("cues"),
                }),
            ],
        };
        let context = fixture.context(ActionPhase::Post);
        let runner = RecordingRunner::default();
        let filesystem = CapabilityActionFilesystem::new();
        let engine = ActionEngine {
            filesystem: &filesystem,
            scripts: &runner,
        };

        engine
            .execute_phase(&pipeline, &context, &NeverCancel)
            .unwrap();

        assert_eq!(fs::read(external_parent.join("logs/one.log")).unwrap(), b"log");
        assert_eq!(fs::read(external_parent.join("cues/two.cue")).unwrap(), b"cue");
        let records = filesystem.scope_records().unwrap();
        assert_eq!(
            records
                .iter()
                .filter(|record| record.logical_path == external_parent)
                .count(),
            1
        );
        assert!(records
            .iter()
            .all(|record| record.logical_path != external_parent.join("logs")));
        assert!(records
            .iter()
            .all(|record| record.logical_path != external_parent.join("cues")));
    }

    fn sibling_missing_destination_pipeline() -> ActionPipeline {
        ActionPipeline {
            pre: Vec::new(),
            post: vec![
                ConversionAction::Copy(CopyAction {
                    targeting: targeting(&["one.log"]),
                    destination: PathBuf::from("exports/logs"),
                }),
                ConversionAction::Copy(CopyAction {
                    targeting: targeting(&["two.cue"]),
                    destination: PathBuf::from("exports/cues"),
                }),
            ],
        }
    }

    #[test]
    fn sibling_destinations_share_one_missing_parent_materialization_authority() {
        let fixture = Fixture::new();
        fs::write(fixture.album_dir.join("one.log"), b"log").unwrap();
        fs::write(fixture.album_dir.join("two.cue"), b"cue").unwrap();
        let pipeline = sibling_missing_destination_pipeline();
        let context = fixture.context(ActionPhase::Post);
        let runner = RecordingRunner::default();
        let filesystem = CapabilityActionFilesystem::new();
        let engine = ActionEngine {
            filesystem: &filesystem,
            scripts: &runner,
        };

        engine
            .execute_phase(&pipeline, &context, &NeverCancel)
            .unwrap();

        assert_eq!(
            fs::read(fixture.album_dir.join("exports/logs/one.log")).unwrap(),
            b"log"
        );
        assert_eq!(
            fs::read(fixture.album_dir.join("exports/cues/two.cue")).unwrap(),
            b"cue"
        );

        let shared_root = fixture.album_dir.join("exports");
        let records = filesystem.scope_records().unwrap();
        let shared: Vec<_> = records
            .iter()
            .filter(|record| record.logical_path == shared_root)
            .collect();
        assert_eq!(shared.len(), 1, "exports must have one durable authority");
        assert!(shared[0].materialization_token.is_some());
        assert!(shared[0].materialized_device.is_some());
        assert!(shared[0].materialized_inode.is_some());
        assert!(records
            .iter()
            .all(|record| record.logical_path != fixture.album_dir.join("exports/logs")));
        assert!(records
            .iter()
            .all(|record| record.logical_path != fixture.album_dir.join("exports/cues")));
    }

    #[test]
    fn sibling_destination_recovery_reuses_materialized_parent_authority() {
        let mut interrupted = None;

        // Select a persistence boundary that represents a real application
        // crash after the first copy has published its sibling but before the
        // second copy has done so. The exact generation count is deliberately
        // not hard-coded because adding an earlier durability fence must not
        // silently weaken this regression.
        for fail_at in 1..=96 {
            let fixture = Fixture::new();
            fs::write(fixture.album_dir.join("one.log"), b"log").unwrap();
            fs::write(fixture.album_dir.join("two.cue"), b"cue").unwrap();
            let pipeline = sibling_missing_destination_pipeline();
            let context = fixture.context(ActionPhase::Post);
            let runner = RecordingRunner::default();
            let filesystem = CapabilityActionFilesystem::new();
            let engine = ActionEngine {
                filesystem: &filesystem,
                scripts: &runner,
            };

            test_set_journal_persist_fault(Some(fail_at));
            let result = engine.execute_phase(&pipeline, &context, &NeverCancel);
            test_set_journal_persist_fault(None);

            let first = fixture.album_dir.join("exports/logs/one.log");
            let second = fixture.album_dir.join("exports/cues/two.cue");
            if result.is_err() && first.is_file() && !second.exists() {
                interrupted = Some((fixture, pipeline, context));
                break;
            }
        }

        let (fixture, pipeline, context) = interrupted.expect(
            "fault injection must find a durable crash boundary between sibling actions",
        );

        // A fresh filesystem registry models process restart. It must restore
        // the one journaled `exports` scope and derive both child destinations
        // from that authority; it must not manufacture a token for `cues`.
        let runner = RecordingRunner::default();
        let recovered_filesystem = CapabilityActionFilesystem::new();
        let recovered_engine = ActionEngine {
            filesystem: &recovered_filesystem,
            scripts: &runner,
        };
        let report = recovered_engine
            .execute_phase(&pipeline, &context, &NeverCancel)
            .unwrap();
        assert!(!report.recovery_required);
        assert_eq!(
            fs::read(fixture.album_dir.join("exports/logs/one.log")).unwrap(),
            b"log"
        );
        assert_eq!(
            fs::read(fixture.album_dir.join("exports/cues/two.cue")).unwrap(),
            b"cue"
        );

        let serialized = pipeline.canonical_serialization().unwrap();
        let journal_path =
            action_journal_path(&context, &sha256_hex(serialized.as_bytes())).unwrap();
        let journal = deserialize_action_journal(&fs::read(journal_path).unwrap()).unwrap();
        let shared_root = fixture.album_dir.join("exports");
        assert_eq!(
            journal
                .capability_roots
                .iter()
                .filter(|record| record.logical_path == shared_root)
                .count(),
            1
        );
        assert!(journal.capability_roots.iter().all(|record| {
            record.logical_path != fixture.album_dir.join("exports/logs")
                && record.logical_path != fixture.album_dir.join("exports/cues")
        }));
    }

    #[test]
    fn same_filesystem_move_uses_descriptor_relative_direct_rename() {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;

        let fixture = Fixture::new();
        let source = fixture.album_dir.join("direct.txt");
        fs::write(&source, b"direct").unwrap();
        #[cfg(unix)]
        let source_inode = fs::metadata(&source).unwrap().ino();
        let pipeline = ActionPipeline {
            pre: Vec::new(),
            post: vec![ConversionAction::Move(MoveAction {
                targeting: targeting(&["direct.txt"]),
                destination: PathBuf::from("Moved"),
            })],
        };
        let context = fixture.context(ActionPhase::Post);
        let runner = RecordingRunner::default();
        let filesystem = CapabilityActionFilesystem::new();
        let engine = ActionEngine {
            filesystem: &filesystem,
            scripts: &runner,
        };
        engine.execute_phase(&pipeline, &context, &NeverCancel).unwrap();
        let destination = fixture.album_dir.join("Moved/direct.txt");
        assert!(!source.exists());
        assert_eq!(fs::read(&destination).unwrap(), b"direct");
        #[cfg(unix)]
        assert_eq!(fs::metadata(destination).unwrap().ino(), source_inode);
    }

    #[test]
    fn forced_exdev_move_uses_copy_verify_publish_remove_state_machine() {
        let fixture = Fixture::new();
        let source = fixture.album_dir.join("cross.txt");
        fs::write(&source, b"cross-device").unwrap();
        let pipeline = ActionPipeline {
            pre: Vec::new(),
            post: vec![ConversionAction::Move(MoveAction {
                targeting: targeting(&["cross.txt"]),
                destination: PathBuf::from("Moved"),
            })],
        };
        let context = fixture.context(ActionPhase::Post);
        let runner = RecordingRunner::default();
        let filesystem = CapabilityActionFilesystem::new();
        filesystem.capabilities.set_force_rename_exdev(true);
        let engine = ActionEngine {
            filesystem: &filesystem,
            scripts: &runner,
        };
        let report = engine.execute_phase(&pipeline, &context, &NeverCancel).unwrap();
        assert!(!report.recovery_required);
        assert!(!source.exists());
        assert_eq!(
            fs::read(fixture.album_dir.join("Moved/cross.txt")).unwrap(),
            b"cross-device"
        );
    }

    #[test]
    fn journal_capability_path_traversal_is_rejected_before_recovery_mutation() {
        let fixture = Fixture::new();
        fs::write(fixture.album_dir.join("safe.txt"), b"safe").unwrap();
        let pipeline = ActionPipeline {
            pre: Vec::new(),
            post: vec![ConversionAction::Copy(CopyAction {
                targeting: targeting(&["safe.txt"]),
                destination: PathBuf::from("Copies"),
            })],
        };
        let context = fixture.context(ActionPhase::Post);
        let runner = RecordingRunner::default();
        test_set_journal_persist_fault(Some(3));
        let _ = engine(&runner).execute_phase(&pipeline, &context, &NeverCancel);
        test_set_journal_persist_fault(None);
        let serialized = pipeline.canonical_serialization().unwrap();
        let journal_path =
            action_journal_path(&context, &sha256_hex(serialized.as_bytes())).unwrap();
        let mut json: serde_json::Value =
            serde_json::from_slice(&fs::read(&journal_path).unwrap()).unwrap();
        json["actions"][0]["operations"][0]["capability_paths"][1]["relative"] =
            serde_json::Value::String("../escape".to_string());
        fs::write(&journal_path, serde_json::to_vec_pretty(&json).unwrap()).unwrap();
        let result = engine(&runner).execute_phase(&pipeline, &context, &NeverCancel);
        assert!(matches!(result, Err(ActionError::Serialization(_))));
        assert!(!fixture._temp.path().join("escape").exists());
        assert_eq!(fs::read(fixture.album_dir.join("safe.txt")).unwrap(), b"safe");
    }


    #[test]
    fn journal_bootstrap_prefers_and_canonicalizes_the_newer_validated_write_temporary() {
        let fixture = Fixture::new();
        let pipeline = ActionPipeline {
            pre: Vec::new(),
            post: vec![ConversionAction::CreateFolder(CreateFolderAction {
                path: PathBuf::from("Created"),
                continue_on_error: false,
            })],
        };
        let context = fixture.context(ActionPhase::Post);
        let runner = RecordingRunner::default();
        let filesystem = CapabilityActionFilesystem::new();
        let engine = ActionEngine {
            filesystem: &filesystem,
            scripts: &runner,
        };
        engine.execute_phase(&pipeline, &context, &NeverCancel).unwrap();

        let serialized = pipeline.canonical_serialization().unwrap();
        let journal_path =
            action_journal_path(&context, &sha256_hex(serialized.as_bytes())).unwrap();
        let temporary_path = journal_write_temporary_path(&journal_path).unwrap();
        let mut newer = deserialize_action_journal(&fs::read(&journal_path).unwrap()).unwrap();
        let final_generation = newer.generation;
        newer.generation += 1;
        fs::write(&temporary_path, serde_json::to_vec_pretty(&newer).unwrap()).unwrap();

        let store = JournalStore::new(journal_path.clone(), &filesystem).unwrap();
        let (mut loaded, from_temporary) = load_journal_bootstrap(&store)
            .unwrap()
            .expect("journal generation should be found");
        assert!(from_temporary);
        assert_eq!(loaded.generation, final_generation + 1);

        store.reconcile_loaded(&loaded).unwrap();
        store.persist(&mut loaded).unwrap();
        assert!(!temporary_path.exists());
        let canonical = deserialize_action_journal(&fs::read(journal_path).unwrap()).unwrap();
        assert_eq!(canonical.generation, final_generation + 2);
    }

    #[test]
    fn equal_generation_journal_and_temporary_with_different_contents_fail_closed() {
        let fixture = Fixture::new();
        let pipeline = ActionPipeline {
            pre: Vec::new(),
            post: vec![ConversionAction::CreateFolder(CreateFolderAction {
                path: PathBuf::from("Created"),
                continue_on_error: false,
            })],
        };
        let context = fixture.context(ActionPhase::Post);
        let runner = RecordingRunner::default();
        let filesystem = CapabilityActionFilesystem::new();
        let engine = ActionEngine {
            filesystem: &filesystem,
            scripts: &runner,
        };
        engine.execute_phase(&pipeline, &context, &NeverCancel).unwrap();

        let serialized = pipeline.canonical_serialization().unwrap();
        let journal_path =
            action_journal_path(&context, &sha256_hex(serialized.as_bytes())).unwrap();
        let temporary_path = journal_write_temporary_path(&journal_path).unwrap();
        let mut contradictory =
            deserialize_action_journal(&fs::read(&journal_path).unwrap()).unwrap();
        contradictory.cancellation.requested = !contradictory.cancellation.requested;
        fs::write(
            &temporary_path,
            serde_json::to_vec_pretty(&contradictory).unwrap(),
        )
        .unwrap();
        let store = JournalStore::new(journal_path, &filesystem).unwrap();
        assert!(matches!(
            load_journal_bootstrap(&store),
            Err(ActionError::Contradiction(_))
        ));
        assert!(temporary_path.exists());
    }

    #[test]
    fn stable_object_authority_survives_cleanup_induced_timestamp_changes() {
        let expected = ObjectIdentity {
            kind: ObjectKind::File,
            content_sha256: "00".repeat(32),
            byte_length: 7,
            entry_count: 1,
            copy_metadata: CopyMetadataIdentity {
                root: CopyMetadataEntry {
                    relative_path: PathBuf::new(),
                    kind: ObjectKind::File,
                    mode: 0o644,
                    modified_nanos: 30,
                },
                descendants: Vec::new(),
            },
            filesystem: FilesystemIdentity {
                device: Some(10),
                inode: Some(20),
                length: 7,
                modified_nanos: Some(30),
                changed_nanos: Some(40),
            },
        };
        let mut after_cleanup = expected.clone();
        after_cleanup.filesystem.modified_nanos = Some(31);
        after_cleanup.filesystem.changed_nanos = Some(41);
        assert!(!after_cleanup.same_object(&expected));
        verify_same_filesystem_object_authority(
            &after_cleanup,
            &expected,
            Path::new("/display-only"),
        )
        .unwrap();

        after_cleanup.filesystem.inode = Some(21);
        assert!(matches!(
            verify_same_filesystem_object_authority(
                &after_cleanup,
                &expected,
                Path::new("/display-only"),
            ),
            Err(ActionError::Contradiction(_))
        ));
    }

    #[test]
    fn legacy_pathname_journal_is_refused_explicitly_and_not_reinterpreted() {
        let error = deserialize_action_journal(br#"{"schema_version":2}"#)
            .expect_err("schema 2 must not be applied through descriptor recovery");
        assert!(matches!(error, ActionError::InvalidJournal(_)));
        assert!(error.to_string().contains("pathname-authority"));
    }

    fn explicit_fixture_context(fixture: &Fixture, marker: char) -> ActionContext {
        let checksum: String = std::iter::repeat(marker).take(64).collect();
        let mut context = fixture.context(ActionPhase::Post);
        context.explicit_scope = true;
        context.source_path = fixture.album_dir.clone();
        context.source_is_directory = true;
        context.subject_dir = fixture.album_dir.clone();
        context.protected_sources.clear();
        context.protected_generated_paths = [
            fixture.album_dir.join(".tonepoet-action-identity.json"),
            fixture.album_dir.join(".tonepoet-actions-manual"),
        ]
        .into_iter()
        .collect();
        context.journal_dir = fixture.album_dir.join(".tonepoet-actions-manual");
        context.run_identity = format!("manual-published:{checksum}");
        context.album_identity = format!("published:{checksum}");
        context
    }

    #[test]
    fn relative_destination_scope_is_derived_from_retained_album_capability() {
        let fixture = Fixture::new();
        let retained_output = fixture._temp.path().join("output-retained");
        let destination_root = fixture.album_dir.join("backup");
        let destination_file = destination_root.join("copied.flac");
        let mut context = fixture.context(ActionPhase::Post);
        context.retained_album_capability = Some(Arc::new(
            PinnedDirectoryCapability::open_trusted(&fixture.album_dir).unwrap(),
        ));

        let filesystem = CapabilityActionFilesystem::new();
        pin_rendered_destination_root(&filesystem, &context, 0, &destination_root).unwrap();

        fs::rename(&fixture.output_root, &retained_output).unwrap();
        fs::create_dir_all(&fixture.album_dir).unwrap();

        filesystem
            .materialize_root_for_path(&destination_file, 0o755)
            .unwrap();
        filesystem
            .write_bytes_create_new_durable(&destination_file, b"retained")
            .unwrap();

        assert_eq!(
            fs::read(retained_output.join("Artist/Album/backup/copied.flac")).unwrap(),
            b"retained"
        );
        assert!(!fixture.album_dir.join("backup/copied.flac").exists());
    }

    #[test]
    fn prepared_manual_execution_uses_byte_identical_reviewed_plans() {
        let fixture = Fixture::new();
        fs::write(fixture.album_dir.join("remove.log"), b"reviewed").unwrap();
        let context = explicit_fixture_context(&fixture, 'a');
        let pipeline = ActionPipeline {
            pre: Vec::new(),
            post: vec![ConversionAction::Delete(DeleteAction {
                targeting: targeting(&["*.log"]),
            })],
        };
        let runner = RecordingRunner::default();
        let filesystem = CapabilityActionFilesystem::new();
        let action_engine = ActionEngine { filesystem: &filesystem, scripts: &runner };
        let mut lock = acquire_explicit_action_run_lock(&context).unwrap();
        let prepared = action_engine
            .prepare_explicit_invocation_with_lock(&pipeline, &context, &"a".repeat(64), &lock)
            .unwrap();
        assert_eq!(
            prepared.plans_serialized,
            serde_json::to_string(&prepared.plans).unwrap()
        );
        action_engine
            .execute_prepared_explicit_phase_with_lock(
                &pipeline,
                &context,
                &"a".repeat(64),
                &prepared.invocation_id,
                &prepared.authority_sha256,
                &NeverCancel,
                &mut lock,
            )
            .unwrap();
        assert!(!fixture.album_dir.join("remove.log").exists());
        assert!(!explicit_preview_path(&context).exists());
    }

    #[test]
    fn prepared_explicit_execution_rebinds_unbound_context_to_locked_album_object() {
        let fixture = Fixture::new();
        let retained_output = fixture._temp.path().join("output-retained");
        fs::write(fixture.album_dir.join("a-only.log"), b"original").unwrap();
        let context = explicit_fixture_context(&fixture, 'q');
        assert!(context.retained_album_capability.is_none());
        assert!(context.retained_output_capability.is_none());
        assert!(context.retained_journal_capability.is_none());
        let pipeline = ActionPipeline {
            pre: Vec::new(),
            post: vec![ConversionAction::Delete(DeleteAction {
                targeting: targeting(&["*.log"]),
            })],
        };
        let runner = RecordingRunner::default();
        let filesystem = CapabilityActionFilesystem::new();
        let engine = ActionEngine {
            filesystem: &filesystem,
            scripts: &runner,
        };
        let mut lock = acquire_explicit_action_run_lock(&context).unwrap();
        let identity_sha256 = "q".repeat(64);
        let prepared = engine
            .prepare_explicit_invocation_with_lock(
                &pipeline,
                &context,
                &identity_sha256,
                &lock,
            )
            .unwrap();

        fs::rename(&fixture.output_root, &retained_output).unwrap();
        fs::create_dir_all(&fixture.album_dir).unwrap();
        fs::write(fixture.album_dir.join("b-only.log"), b"replacement").unwrap();

        engine
            .execute_prepared_explicit_phase_with_lock(
                &pipeline,
                &context,
                &identity_sha256,
                &prepared.invocation_id,
                &prepared.authority_sha256,
                &NeverCancel,
                &mut lock,
            )
            .unwrap();

        assert!(!retained_output.join("Artist/Album/a-only.log").exists());
        assert_eq!(
            fs::read(fixture.album_dir.join("b-only.log")).unwrap(),
            b"replacement"
        );
        assert!(!fixture.album_dir.join("a-only.log").exists());
    }

    #[cfg(unix)]
    #[test]
    fn manual_runscript_releases_publication_lock_but_keeps_action_exclusion() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = Fixture::new();
        let script = fixture._temp.path().join("publication-lock-probe.sh");
        fs::write(&script, b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let context = explicit_fixture_context(&fixture, 'p');
        let pipeline = ActionPipeline {
            pre: Vec::new(),
            post: vec![ConversionAction::Runscript(RunScriptAction {
                script,
                args: Vec::new(),
                timeout_seconds: 30,
                continue_on_error: false,
            })],
        };
        let probed = Arc::new(Mutex::new(false));
        let runner = PublicationLockProbeRunner {
            album_dir: fixture.album_dir.clone(),
            probed: Arc::clone(&probed),
        };
        let filesystem = CapabilityActionFilesystem::new();
        let engine = ActionEngine { filesystem: &filesystem, scripts: &runner };
        let mut lock = acquire_explicit_action_run_lock(&context).unwrap();
        let prepared = engine
            .prepare_explicit_invocation_with_lock(
                &pipeline,
                &context,
                &"9".repeat(64),
                &lock,
            )
            .unwrap();

        engine
            .execute_prepared_explicit_phase_with_lock(
                &pipeline,
                &context,
                &"9".repeat(64),
                &prepared.invocation_id,
                &prepared.authority_sha256,
                &NeverCancel,
                &mut lock,
            )
            .unwrap();
        assert!(*probed.lock().unwrap());
        assert!(lock.holds_action_execution_authority());
    }

    #[cfg(unix)]
    #[test]
    fn prepared_runscript_replacement_is_refused_before_runner_launch() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = Fixture::new();
        let script = fixture._temp.path().join("reviewed-action.sh");
        fs::write(&script, b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let context = explicit_fixture_context(&fixture, 'd');
        let pipeline = ActionPipeline {
            pre: Vec::new(),
            post: vec![ConversionAction::Runscript(RunScriptAction {
                script: script.clone(),
                args: Vec::new(),
                timeout_seconds: 30,
                continue_on_error: false,
            })],
        };
        let runner = RecordingRunner::default();
        let filesystem = CapabilityActionFilesystem::new();
        let action_engine = ActionEngine {
            filesystem: &filesystem,
            scripts: &runner,
        };
        let lock = acquire_explicit_action_run_lock(&context).unwrap();
        let prepared = action_engine
            .prepare_explicit_invocation_with_lock(
                &pipeline,
                &context,
                &"d".repeat(64),
                &lock,
            )
            .unwrap();
        drop(lock);

        let replacement = fixture._temp.path().join("replacement-action.sh");
        fs::write(&replacement, b"#!/bin/sh\nexit 42\n").unwrap();
        fs::set_permissions(&replacement, fs::Permissions::from_mode(0o755)).unwrap();
        fs::rename(&replacement, &script).unwrap();

        let execution_filesystem = CapabilityActionFilesystem::new();
        let execution_engine = ActionEngine {
            filesystem: &execution_filesystem,
            scripts: &runner,
        };
        let mut lock = acquire_explicit_action_run_lock(&context).unwrap();
        assert!(matches!(
            execution_engine.execute_prepared_explicit_phase_with_lock(
                &pipeline,
                &context,
                &"d".repeat(64),
                &prepared.invocation_id,
                &prepared.authority_sha256,
                &NeverCancel,
                &mut lock,
            ),
            Err(ActionError::PreviewStale(_))
        ));
        assert_eq!(runner.calls.lock().unwrap().len(), 0);
        assert!(!explicit_active_run_path(&context).exists());
    }

    #[test]
    fn matching_file_added_after_preview_causes_zero_mutation_stale_refusal() {
        let fixture = Fixture::new();
        let first = fixture.album_dir.join("first.log");
        let second = fixture.album_dir.join("second.log");
        fs::write(&first, b"first").unwrap();
        let context = explicit_fixture_context(&fixture, 'b');
        let pipeline = ActionPipeline {
            pre: Vec::new(),
            post: vec![ConversionAction::Delete(DeleteAction {
                targeting: targeting(&["*.log"]),
            })],
        };
        let runner = RecordingRunner::default();
        let filesystem = CapabilityActionFilesystem::new();
        let action_engine = ActionEngine { filesystem: &filesystem, scripts: &runner };
        let lock = acquire_explicit_action_run_lock(&context).unwrap();
        let prepared = action_engine
            .prepare_explicit_invocation_with_lock(&pipeline, &context, &"b".repeat(64), &lock)
            .unwrap();
        drop(lock);
        fs::write(&second, b"second").unwrap();

        let execution_filesystem = CapabilityActionFilesystem::new();
        let execution_engine = ActionEngine { filesystem: &execution_filesystem, scripts: &runner };
        let mut lock = acquire_explicit_action_run_lock(&context).unwrap();
        assert!(matches!(
            execution_engine.execute_prepared_explicit_phase_with_lock(
                &pipeline,
                &context,
                &"b".repeat(64),
                &prepared.invocation_id,
                &prepared.authority_sha256,
                &NeverCancel,
                &mut lock,
            ),
            Err(ActionError::PreviewStale(_))
        ));
        assert!(first.exists());
        assert!(second.exists());
        assert!(!explicit_active_run_path(&context).exists());
    }

    #[test]
    fn canonical_identity_change_after_preview_causes_stale_refusal() {
        let fixture = Fixture::new();
        let context = explicit_fixture_context(&fixture, 'c');
        let pipeline = ActionPipeline {
            pre: Vec::new(),
            post: vec![ConversionAction::CreateFolder(CreateFolderAction {
                path: PathBuf::from("Created"),
                continue_on_error: false,
            })],
        };
        let runner = RecordingRunner::default();
        let filesystem = CapabilityActionFilesystem::new();
        let action_engine = ActionEngine { filesystem: &filesystem, scripts: &runner };
        let mut lock = acquire_explicit_action_run_lock(&context).unwrap();
        let prepared = action_engine
            .prepare_explicit_invocation_with_lock(&pipeline, &context, &"c".repeat(64), &lock)
            .unwrap();
        assert!(matches!(
            action_engine.execute_prepared_explicit_phase_with_lock(
                &pipeline,
                &context,
                &"d".repeat(64),
                &prepared.invocation_id,
                &prepared.authority_sha256,
                &NeverCancel,
                &mut lock,
            ),
            Err(ActionError::PreviewStale(_))
        ));
        assert!(!fixture.album_dir.join("Created").exists());
    }

    #[test]
    fn cancellation_before_prepared_execution_creates_no_active_or_journal_authority() {
        struct Cancelled;
        impl ActionCancellation for Cancelled {
            fn is_cancelled(&self) -> bool { true }
        }
        let fixture = Fixture::new();
        let context = explicit_fixture_context(&fixture, 'e');
        let pipeline = ActionPipeline {
            pre: Vec::new(),
            post: vec![ConversionAction::CreateFolder(CreateFolderAction {
                path: PathBuf::from("Never"),
                continue_on_error: false,
            })],
        };
        let runner = RecordingRunner::default();
        let filesystem = CapabilityActionFilesystem::new();
        let action_engine = ActionEngine { filesystem: &filesystem, scripts: &runner };
        let mut lock = acquire_explicit_action_run_lock(&context).unwrap();
        let prepared = action_engine
            .prepare_explicit_invocation_with_lock(&pipeline, &context, &"e".repeat(64), &lock)
            .unwrap();
        assert!(matches!(
            action_engine.execute_prepared_explicit_phase_with_lock(
                &pipeline,
                &context,
                &"e".repeat(64),
                &prepared.invocation_id,
                &prepared.authority_sha256,
                &Cancelled,
                &mut lock,
            ),
            Err(ActionError::CancelledBeforeMutation(_))
        ));
        assert!(!fixture.album_dir.join("Never").exists());
        assert!(!explicit_active_run_path(&context).exists());
        assert!(!explicit_preview_path(&context).exists());
        let digest = pipeline.canonical_sha256().unwrap();
        let mut execution_context = context.clone();
        execution_context.run_identity = prepared.invocation_id;
        assert!(!action_journal_path(&execution_context, &digest).unwrap().exists());
    }

    #[test]
    fn refreshed_preview_gets_a_new_invocation_identity() {
        let fixture = Fixture::new();
        let context = explicit_fixture_context(&fixture, 'f');
        let pipeline = ActionPipeline {
            pre: Vec::new(),
            post: vec![ConversionAction::CreateFolder(CreateFolderAction {
                path: PathBuf::from("Previewed"),
                continue_on_error: false,
            })],
        };
        let runner = RecordingRunner::default();
        let filesystem = CapabilityActionFilesystem::new();
        let action_engine = ActionEngine { filesystem: &filesystem, scripts: &runner };
        let lock = acquire_explicit_action_run_lock(&context).unwrap();
        let first = action_engine
            .prepare_explicit_invocation_with_lock(&pipeline, &context, &"f".repeat(64), &lock)
            .unwrap();
        action_engine
            .discard_prepared_explicit_preview_with_lock(
                &context,
                &first.invocation_id,
                &first.authority_sha256,
                &lock,
            )
            .unwrap();
        let second = action_engine
            .prepare_explicit_invocation_with_lock(&pipeline, &context, &"f".repeat(64), &lock)
            .unwrap();
        assert_ne!(first.invocation_id, second.invocation_id);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_aliases_share_one_parent_capability_authority() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("real");
        let album = real.join("album");
        fs::create_dir_all(&album).unwrap();
        let alias_one = temp.path().join("alias-one");
        let alias_two = temp.path().join("alias-two");
        symlink(&real, &alias_one).unwrap();
        symlink(&real, &alias_two).unwrap();
        let first = acquire_explicit_action_run_lock_for_album(&alias_one.join("album")).unwrap();
        assert!(matches!(
            acquire_explicit_action_run_lock_for_album(&alias_two.join("album")),
            Err(ActionError::Conflict(_))
        ));
        drop(first);
        acquire_explicit_action_run_lock_for_album(&alias_two.join("album")).unwrap();
    }



    #[cfg(unix)]
    #[test]
    fn normalized_and_non_normalized_album_paths_share_one_authority() {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().join("parent");
        let intermediate = parent.join("intermediate");
        let album = parent.join("album");
        fs::create_dir_all(&intermediate).unwrap();
        fs::create_dir_all(&album).unwrap();

        let first = acquire_explicit_action_run_lock_for_album(&album).unwrap();
        let aliased = intermediate.join("..").join("album");
        assert!(matches!(
            acquire_explicit_action_run_lock_for_album(&aliased),
            Err(ActionError::Conflict(_))
        ));
        drop(first);
        acquire_explicit_action_run_lock_for_album(&aliased).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn interrupted_manual_journal_is_recovered_through_a_symlink_alias() {
        use std::os::unix::fs::symlink;

        let fixture = Fixture::new();
        let source = fixture.album_dir.join("alias-move.txt");
        fs::write(&source, b"alias-recovery").unwrap();
        let context = explicit_fixture_context(&fixture, '6');
        let pipeline = ActionPipeline {
            pre: Vec::new(),
            post: vec![ConversionAction::Move(MoveAction {
                targeting: targeting(&["alias-move.txt"]),
                destination: PathBuf::from("Recovered"),
            })],
        };
        let alias_one = fixture._temp.path().join("output-alias-one");
        let alias_two = fixture._temp.path().join("output-alias-two");
        symlink(&fixture.output_root, &alias_one).unwrap();
        symlink(&fixture.output_root, &alias_two).unwrap();
        let alias_album_one = alias_one.join("Artist/Album");
        let alias_album_two = alias_two.join("Artist/Album");

        let runner = RecordingRunner::default();
        let filesystem = CapabilityActionFilesystem::new();
        let action_engine = ActionEngine { filesystem: &filesystem, scripts: &runner };
        let mut lock = acquire_explicit_action_run_lock_for_album(&alias_album_one).unwrap();
        let fresh = action_engine
            .prepare_explicit_invocation_with_lock(&pipeline, &context, &"6".repeat(64), &lock)
            .unwrap();
        test_set_journal_persist_fault(Some(7));
        let interrupted = action_engine.execute_prepared_explicit_phase_with_lock(
            &pipeline,
            &context,
            &"6".repeat(64),
            &fresh.invocation_id,
            &fresh.authority_sha256,
            &NeverCancel,
            &mut lock,
        );
        test_set_journal_persist_fault(None);
        assert!(interrupted.is_err());
        drop(lock);

        let recovery_filesystem = CapabilityActionFilesystem::new();
        let recovery_engine = ActionEngine { filesystem: &recovery_filesystem, scripts: &runner };
        let mut recovery_lock = acquire_explicit_action_run_lock_for_album(&alias_album_two).unwrap();
        let recovery = recovery_engine
            .prepare_explicit_invocation_with_lock(
                &pipeline,
                &context,
                &"6".repeat(64),
                &recovery_lock,
            )
            .unwrap();
        assert!(recovery.is_recovery);
        assert_eq!(recovery.invocation_id, fresh.invocation_id);
        recovery_engine
            .execute_prepared_explicit_phase_with_lock(
                &pipeline,
                &context,
                &"6".repeat(64),
                &recovery.invocation_id,
                &recovery.authority_sha256,
                &NeverCancel,
                &mut recovery_lock,
            )
            .unwrap();
        assert_eq!(
            fs::read(fixture.album_dir.join("Recovered/alias-move.txt")).unwrap(),
            b"alias-recovery"
        );
    }

    #[test]
    fn recomputed_preview_checksum_cannot_authorize_semantically_tampered_plan() {
        let fixture = Fixture::new();
        let context = explicit_fixture_context(&fixture, '0');
        let pipeline = ActionPipeline {
            pre: Vec::new(),
            post: vec![ConversionAction::CreateFolder(CreateFolderAction {
                path: PathBuf::from("Reviewed"),
                continue_on_error: false,
            })],
        };
        let runner = RecordingRunner::default();
        let filesystem = CapabilityActionFilesystem::new();
        let action_engine = ActionEngine { filesystem: &filesystem, scripts: &runner };
        let mut lock = acquire_explicit_action_run_lock(&context).unwrap();
        let prepared = action_engine
            .prepare_explicit_invocation_with_lock(&pipeline, &context, &"0".repeat(64), &lock)
            .unwrap();
        let (original, _) = load_explicit_preview_locked(&context, &lock)
            .unwrap()
            .expect("preview authority");
        let mut plans: Vec<ActionPlan> =
            serde_json::from_str(&original.payload.plans_serialized).unwrap();
        plans[0].operations = vec![PlannedOperation::CreateDirectory {
            path: fixture.album_dir.join(".tonepoet-action-identity.json"),
        }];
        let plans_serialized = serde_json::to_string(&plans).unwrap();
        let mut payload = original.payload.clone();
        payload.generation += 1;
        payload.plans_sha256 = sha256_hex(plans_serialized.as_bytes());
        payload.plans_serialized = plans_serialized;
        let tampered = explicit_preview_record(payload).unwrap();
        write_explicit_preview_locked(&tampered, Some(&original), &context, &lock).unwrap();
        let tampered_binding = explicit_preview_binding_sha256(&tampered.payload).unwrap();

        assert!(matches!(
            action_engine.execute_prepared_explicit_phase_with_lock(
                &pipeline,
                &context,
                &"0".repeat(64),
                &prepared.invocation_id,
                &tampered_binding,
                &NeverCancel,
                &mut lock,
            ),
            Err(ActionError::InvalidJournal(_)) | Err(ActionError::UnsafePath(_))
        ));
        assert!(!fixture.album_dir.join("Reviewed").exists());
        assert!(!fixture.album_dir.join(".tonepoet-action-identity.json").exists());
        assert!(!explicit_active_run_path(&context).exists());
    }

    #[test]
    fn matching_file_removed_after_preview_causes_zero_mutation_stale_refusal() {
        let fixture = Fixture::new();
        let first = fixture.album_dir.join("first.log");
        let survivor = fixture.album_dir.join("survivor.txt");
        fs::write(&first, b"first").unwrap();
        fs::write(&survivor, b"survivor").unwrap();
        let context = explicit_fixture_context(&fixture, '1');
        let pipeline = ActionPipeline {
            pre: Vec::new(),
            post: vec![ConversionAction::Delete(DeleteAction {
                targeting: targeting(&["*.log"]),
            })],
        };
        let runner = RecordingRunner::default();
        let filesystem = CapabilityActionFilesystem::new();
        let action_engine = ActionEngine { filesystem: &filesystem, scripts: &runner };
        let lock = acquire_explicit_action_run_lock(&context).unwrap();
        let prepared = action_engine
            .prepare_explicit_invocation_with_lock(&pipeline, &context, &"1".repeat(64), &lock)
            .unwrap();
        drop(lock);
        fs::remove_file(&first).unwrap();

        let execution_filesystem = CapabilityActionFilesystem::new();
        let execution_engine = ActionEngine { filesystem: &execution_filesystem, scripts: &runner };
        let mut lock = acquire_explicit_action_run_lock(&context).unwrap();
        assert!(matches!(
            execution_engine.execute_prepared_explicit_phase_with_lock(
                &pipeline,
                &context,
                &"1".repeat(64),
                &prepared.invocation_id,
                &prepared.authority_sha256,
                &NeverCancel,
                &mut lock,
            ),
            Err(ActionError::PreviewStale(_))
        ));
        assert_eq!(fs::read(&survivor).unwrap(), b"survivor");
        assert!(!explicit_active_run_path(&context).exists());
    }

    #[test]
    fn pipeline_change_after_preview_causes_zero_mutation_stale_refusal() {
        let fixture = Fixture::new();
        let context = explicit_fixture_context(&fixture, '2');
        let reviewed = ActionPipeline {
            pre: Vec::new(),
            post: vec![ConversionAction::CreateFolder(CreateFolderAction {
                path: PathBuf::from("Reviewed"),
                continue_on_error: false,
            })],
        };
        let changed = ActionPipeline {
            pre: Vec::new(),
            post: vec![ConversionAction::CreateFolder(CreateFolderAction {
                path: PathBuf::from("Changed"),
                continue_on_error: false,
            })],
        };
        let runner = RecordingRunner::default();
        let filesystem = CapabilityActionFilesystem::new();
        let action_engine = ActionEngine { filesystem: &filesystem, scripts: &runner };
        let mut lock = acquire_explicit_action_run_lock(&context).unwrap();
        let prepared = action_engine
            .prepare_explicit_invocation_with_lock(&reviewed, &context, &"2".repeat(64), &lock)
            .unwrap();
        assert!(matches!(
            action_engine.execute_prepared_explicit_phase_with_lock(
                &changed,
                &context,
                &"2".repeat(64),
                &prepared.invocation_id,
                &prepared.authority_sha256,
                &NeverCancel,
                &mut lock,
            ),
            Err(ActionError::PreviewStale(_))
        ));
        assert!(!fixture.album_dir.join("Reviewed").exists());
        assert!(!fixture.album_dir.join("Changed").exists());
    }

    #[test]
    fn replacing_album_root_after_preview_refuses_execution() {
        let fixture = Fixture::new();
        fs::write(fixture.album_dir.join("victim.log"), b"original").unwrap();
        let context = explicit_fixture_context(&fixture, '3');
        let pipeline = ActionPipeline {
            pre: Vec::new(),
            post: vec![ConversionAction::Delete(DeleteAction {
                targeting: targeting(&["*.log"]),
            })],
        };
        let runner = RecordingRunner::default();
        let filesystem = CapabilityActionFilesystem::new();
        let action_engine = ActionEngine { filesystem: &filesystem, scripts: &runner };
        let lock = acquire_explicit_action_run_lock(&context).unwrap();
        let prepared = action_engine
            .prepare_explicit_invocation_with_lock(&pipeline, &context, &"3".repeat(64), &lock)
            .unwrap();
        drop(lock);

        let displaced = fixture.album_dir.with_file_name("Album.displaced");
        fs::rename(&fixture.album_dir, &displaced).unwrap();
        fs::create_dir_all(&fixture.album_dir).unwrap();
        let replacement = fixture.album_dir.join("victim.log");
        fs::write(&replacement, b"replacement").unwrap();

        let execution_filesystem = CapabilityActionFilesystem::new();
        let execution_engine = ActionEngine { filesystem: &execution_filesystem, scripts: &runner };
        let mut lock = acquire_explicit_action_run_lock(&context).unwrap();
        assert!(matches!(
            execution_engine.execute_prepared_explicit_phase_with_lock(
                &pipeline,
                &context,
                &"3".repeat(64),
                &prepared.invocation_id,
                &prepared.authority_sha256,
                &NeverCancel,
                &mut lock,
            ),
            Err(ActionError::PreviewStale(_))
        ));
        assert_eq!(fs::read(&replacement).unwrap(), b"replacement");
        assert_eq!(fs::read(displaced.join("victim.log")).unwrap(), b"original");
    }

    #[test]
    fn interrupted_manual_run_previews_and_resumes_the_durable_original_plan() {
        let fixture = Fixture::new();
        let source = fixture.album_dir.join("move.txt");
        fs::write(&source, b"move-me").unwrap();
        let context = explicit_fixture_context(&fixture, '4');
        let pipeline = ActionPipeline {
            pre: Vec::new(),
            post: vec![ConversionAction::Move(MoveAction {
                targeting: targeting(&["move.txt"]),
                destination: PathBuf::from("Moved"),
            })],
        };
        let runner = RecordingRunner::default();
        let filesystem = CapabilityActionFilesystem::new();
        let action_engine = ActionEngine { filesystem: &filesystem, scripts: &runner };
        let mut lock = acquire_explicit_action_run_lock(&context).unwrap();
        let fresh = action_engine
            .prepare_explicit_invocation_with_lock(&pipeline, &context, &"4".repeat(64), &lock)
            .unwrap();
        test_set_journal_persist_fault(Some(7));
        let interrupted = action_engine.execute_prepared_explicit_phase_with_lock(
            &pipeline,
            &context,
            &"4".repeat(64),
            &fresh.invocation_id,
            &fresh.authority_sha256,
            &NeverCancel,
            &mut lock,
        );
        test_set_journal_persist_fault(None);
        assert!(interrupted.is_err());

        let recovery_filesystem = CapabilityActionFilesystem::new();
        let recovery_engine = ActionEngine { filesystem: &recovery_filesystem, scripts: &runner };
        let recovery = recovery_engine
            .prepare_explicit_invocation_with_lock(&pipeline, &context, &"4".repeat(64), &lock)
            .unwrap();
        assert!(recovery.is_recovery);
        assert_eq!(recovery.invocation_id, fresh.invocation_id);
        assert_eq!(recovery.plans_serialized, fresh.plans_serialized);
        assert!(!recovery.recovery_operations.is_empty());
        assert!(recovery
            .recovery_operations
            .iter()
            .any(|operation| operation.summary.contains("move")));

        recovery_engine
            .execute_prepared_explicit_phase_with_lock(
                &pipeline,
                &context,
                &"4".repeat(64),
                &recovery.invocation_id,
                &recovery.authority_sha256,
                &NeverCancel,
                &mut lock,
            )
            .unwrap();
        assert!(!source.exists());
        assert_eq!(
            fs::read(fixture.album_dir.join("Moved/move.txt")).unwrap(),
            b"move-me"
        );
    }


    #[test]
    fn recovery_refuses_tampered_journal_that_targets_control_plane_authority() {
        let fixture = Fixture::new();
        fs::write(fixture.album_dir.join("ordinary.txt"), b"ordinary").unwrap();
        let identity = fixture.album_dir.join(".tonepoet-action-identity.json");
        fs::write(&identity, b"identity").unwrap();
        let pipeline = ActionPipeline {
            pre: Vec::new(),
            post: vec![ConversionAction::Delete(DeleteAction {
                targeting: targeting(&["*"]),
            })],
        };
        let context = fixture.context(ActionPhase::Post);
        let runner = RecordingRunner::default();
        test_set_journal_persist_fault(Some(3));
        let _ = engine(&runner).execute_phase(&pipeline, &context, &NeverCancel);
        test_set_journal_persist_fault(None);

        let serialized = pipeline.canonical_serialization().unwrap();
        let journal_path = action_journal_path(&context, &sha256_hex(serialized.as_bytes())).unwrap();
        let mut json: serde_json::Value =
            serde_json::from_slice(&fs::read(&journal_path).unwrap()).unwrap();
        let protected = serde_json::Value::String(identity.display().to_string());
        json["actions"][0]["plan"]["operations"][0]["target"] = protected.clone();
        json["actions"][0]["operations"][0]["plan"]["target"] = protected;
        fs::write(&journal_path, serde_json::to_vec_pretty(&json).unwrap()).unwrap();

        let error = engine(&runner)
            .execute_phase(&pipeline, &context, &NeverCancel)
            .unwrap_err();
        assert!(matches!(error, ActionError::InvalidJournal(_)));
        assert_eq!(fs::read(identity).unwrap(), b"identity");
    }

    #[test]
    fn protected_control_plane_paths_are_rejected_for_every_builtin_surface() {
        let fixture = Fixture::new();
        let identity = fixture.album_dir.join(".tonepoet-action-identity.json");
        let manual = fixture.album_dir.join(".tonepoet-actions-manual");
        fs::write(&identity, b"identity").unwrap();
        fs::create_dir_all(&manual).unwrap();
        fs::write(fixture.album_dir.join("ordinary.txt"), b"ordinary").unwrap();
        let context = explicit_fixture_context(&fixture, '5');
        let runner = RecordingRunner::default();
        let action_engine = engine(&runner);

        let cases = vec![
            ConversionAction::Rename(RenameAction {
                targeting: targeting(&[".tonepoet-actions-manual"]),
                mode: RenameMode::Template,
                template: "renamed".to_string(),
            }),
            ConversionAction::Copy(CopyAction {
                targeting: targeting(&["ordinary.txt"]),
                destination: PathBuf::from(".tonepoet-actions-manual"),
            }),
            ConversionAction::Move(MoveAction {
                targeting: targeting(&["ordinary.txt"]),
                destination: PathBuf::from(".tonepoet-action-locks"),
            }),
            ConversionAction::Delete(DeleteAction {
                targeting: targeting(&[".tonepoet-action-identity.json"]),
            }),
            ConversionAction::CreateFolder(CreateFolderAction {
                path: PathBuf::from(".tonepoet-action-identity.json"),
                continue_on_error: false,
            }),
        ];
        for (index, action) in cases.iter().enumerate() {
            assert!(
                matches!(
                    action_engine.plan_action(index, action, &context, "protected-all-builtins"),
                    Err(ActionError::UnsafePath(_))
                ),
                "protected control-plane case {index} was not rejected"
            );
        }
        assert_eq!(fs::read(identity).unwrap(), b"identity");
        assert!(manual.is_dir());
    }

    #[test]
    fn exact_and_wildcard_targets_cannot_mutate_control_plane_identity() {
        let fixture = Fixture::new();
        let identity = fixture.album_dir.join(".tonepoet-action-identity.json");
        fs::write(&identity, b"identity").unwrap();
        let context = explicit_fixture_context(&fixture, '9');
        let runner = RecordingRunner::default();
        let action_engine = engine(&runner);
        let exact = ConversionAction::Delete(DeleteAction {
            targeting: targeting(&[".tonepoet-action-identity.json"]),
        });
        assert!(matches!(
            action_engine.plan_action(0, &exact, &context, "exact-protected"),
            Err(ActionError::UnsafePath(_))
        ));
        let wildcard = ConversionAction::Delete(DeleteAction {
            targeting: targeting(&["*"]),
        });
        let plan = action_engine
            .plan_action(0, &wildcard, &context, "wildcard-protected")
            .unwrap();
        assert!(plan.operations.iter().all(|operation| {
            !operation.all_paths().into_iter().any(|path| path == identity)
        }));
    }

}
