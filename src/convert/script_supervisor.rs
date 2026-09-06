//! Process-tree supervision for conversion action scripts.
//!
//! External work always executes behind a tonepoet-controlled containment process.
//! Queue conversions reuse one long-lived supervisor per active item and fork
//! command-specific containment workers beneath it; non-queue callers may use a
//! fresh dedicated helper. Keeping containment out of the TUI/worker process avoids
//! changing process-global subreaper state there and gives timeout/cancellation
//! ownership to a process which has no unrelated children.
//!
//! Linux prefers a delegated cgroup-v2 leaf and also makes the dedicated
//! helper a child subreaper.  A minimal hidden launcher joins the retained
//! cgroup descriptor before it receives the authenticated invocation, so the
//! target script cannot execute or fork outside the armed containment boundary.
//! If cgroup delegation is unavailable, the subreaper tracks the complete
//! `/proc` descendant graph with PID start-time validation.  On macOS the helper
//! arms EVFILT_PROC/NOTE_FORK before releasing an exec gate and combines those
//! notifications with libproc child enumeration and process start identities.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ffi::{CString, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::os::unix::net::UnixStream;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};
use thiserror::Error;

const STDIO_TAIL_LIMIT: usize = 64 * 1024;
const CONTROL_CANCEL: u8 = b'C';
const CONTROL_TIMEOUT: u8 = b'T';
const CONTROL_PARENT_GONE: u8 = b'P';
const CONTROL_POLL_INTERVAL: Duration = Duration::from_millis(10);
const TERM_GRACE: Duration = Duration::from_millis(750);
const KILL_GRACE: Duration = Duration::from_secs(5);
const SUPERVISOR_RESULT_SCHEMA: u32 = 3;
const LIFECYCLE_EVENT_SCHEMA: u32 = 1;
const INTERNAL_SUBCOMMAND: &str = "__action-script-supervisor";
const INTERNAL_ITEM_SUPERVISOR_SUBCOMMAND: &str = "__execution-item-supervisor";
const ITEM_REQUEST_RUN: u8 = b'R';
const ITEM_REQUEST_LEASE: u8 = b'L';
const ITEM_REQUEST_SHUTDOWN: u8 = b'S';
const ITEM_REQUEST_ACK: u8 = b'A';
const ITEM_MAX_FDS: usize = 8;
const INTERNAL_LAUNCHER_SUBCOMMAND: &str = "__action-script-launcher";
const MAX_LAUNCH_SPEC_BYTES: usize = 1024 * 1024;
const LAUNCHER_READY: u8 = b'R';
const EVENT_ACK: u8 = b'A';
const EVENT_ABORT: u8 = b'X';
const MAX_EVENT_BYTES: usize = 256 * 1024;
const LAUNCHER_READY_TIMEOUT: Duration = Duration::from_secs(2);
const TAIL_DRAIN_GRACE: Duration = Duration::from_millis(100);
const BACKGROUND_EXIT_GRACE: Duration = Duration::from_millis(250);
const RECOVERY_WAIT_GRACE: Duration = Duration::from_secs(5);
const LIFECYCLE_IO_TIMEOUT: Duration = Duration::from_secs(30);
const SPEC_FILE_NAME: &str = "spec.json";
const RESULT_FILE_NAME: &str = "result.json";
const RESULT_TEMP_FILE_NAME: &str = "result.json.tmp";

#[derive(Debug, Error)]
pub enum ScriptSupervisorError {
    #[error("script supervisor I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("script supervisor serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("script supervisor protocol error: {0}")]
    Protocol(String),
    #[error("script supervisor failed: {0}")]
    Internal(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContainmentBackend {
    LinuxCgroupV2,
    LinuxSubreaper,
    MacosSupervisor,
}

impl ContainmentBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LinuxCgroupV2 => "linux_cgroup_v2",
            Self::LinuxSubreaper => "linux_subreaper",
            Self::MacosSupervisor => "macos_supervisor",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ContainmentPreference {
    #[default]
    Auto,
    RequireLinuxCgroupV2,
    ForceSupervisorFallback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContainmentConfidence {
    KernelEnforced,
    ProcessTreeObserved,
}

impl ContainmentConfidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::KernelEnforced => "kernel_enforced",
            Self::ProcessTreeObserved => "process_tree_observed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostBootIdentity {
    pub machine_identity: String,
    pub host_identity: String,
    pub boot_identity: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StableProcessIdentity {
    pub pid: u32,
    pub start_identity: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinuxCgroupIdentity {
    pub absolute_path: PathBuf,
    pub device: u64,
    pub inode: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeDirectoryIdentity {
    pub device: u64,
    pub inode: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainmentDescriptor {
    pub schema_version: u32,
    pub token: String,
    pub backend: ContainmentBackend,
    pub confidence: ContainmentConfidence,
    pub host: HostBootIdentity,
    pub supervisor: StableProcessIdentity,
    pub leader: StableProcessIdentity,
    pub runtime_directory: RuntimeDirectoryIdentity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cgroup: Option<LinuxCgroupIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminationReason {
    Cancellation,
    Timeout,
    LeaderExitedWithDescendants,
    ParentDisconnected,
    Recovery,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputCaptureTerminal {
    Complete,
    Truncated,
    Abandoned,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputCaptureSummary {
    pub stdout: OutputCaptureTerminal,
    pub stderr: OutputCaptureTerminal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ScriptLifecycleEvent {
    ContainmentPrepared {
        schema_version: u32,
        descriptor: ContainmentDescriptor,
    },
    UserCodeReleased {
        schema_version: u32,
        leader: StableProcessIdentity,
    },
    TerminationRequested {
        schema_version: u32,
        reason: TerminationReason,
        graceful_deadline_unix_millis: u64,
    },
    ForcedTerminationRequested {
        schema_version: u32,
        reason: TerminationReason,
    },
    LeaderExited {
        schema_version: u32,
        raw_wait_status: i32,
    },
    ContainmentEmpty {
        schema_version: u32,
        confidence: ContainmentConfidence,
    },
    OutputCaptureCompleted {
        schema_version: u32,
        summary: OutputCaptureSummary,
    },
}

impl ScriptLifecycleEvent {
    fn schema_version(&self) -> u32 {
        match self {
            Self::ContainmentPrepared { schema_version, .. }
            | Self::UserCodeReleased { schema_version, .. }
            | Self::TerminationRequested { schema_version, .. }
            | Self::ForcedTerminationRequested { schema_version, .. }
            | Self::LeaderExited { schema_version, .. }
            | Self::ContainmentEmpty { schema_version, .. }
            | Self::OutputCaptureCompleted { schema_version, .. } => *schema_version,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SupervisedCommand {
    pub token: String,
    pub runtime_directory: PathBuf,
    /// Retained no-follow descriptor for the exact reviewed executable.
    pub script_file: Arc<File>,
    /// Retained descriptor for the exact reviewed working directory. The
    /// trusted supervisor inherits this descriptor and performs `fchdir(2)`
    /// before spawning the launcher, so no later pathname lookup can redirect
    /// script execution.
    pub working_directory_file: Arc<File>,
    /// Original pathname retained only for diagnostics and argv[0].
    pub script: PathBuf,
    pub args: Vec<String>,
    pub working_directory: PathBuf,
    pub environment: BTreeMap<String, String>,
    pub timeout: Duration,
    pub runtime_identity: RuntimeDirectoryIdentity,
    pub containment_preference: ContainmentPreference,
    /// Test/embedding override for the trusted Tonepoet helper binary.
    /// Production callers leave this as `None`, which resolves to the running Tonepoet image.
    pub helper_executable: Option<PathBuf>,
    /// Tonepoet-owned lifetime descriptors retained only by the trusted supervisor.
    /// They are made inheritable for the supervisor handoff and immediately
    /// restored to CLOEXEC there, so launchers and third-party programs never
    /// become ownership anchors.
    pub retained_lifetime_files: Vec<Arc<File>>,
    /// Optional trusted pipe/file endpoints for supervised internal pipelines.
    /// They are installed on the tonepoet supervisor process, then inherited as
    /// ordinary stdio by the contained child. They are not ownership leases.
    pub stdin_file: Option<Arc<File>>,
    pub stdout_file: Option<Arc<File>>,
    pub stderr_file: Option<Arc<File>>,
}

#[derive(Debug, Clone)]
pub struct SupervisedOutcome {
    pub status: ExitStatus,
    pub stdout_tail: Vec<u8>,
    pub stderr_tail: Vec<u8>,
    pub timed_out: bool,
    pub cancelled: bool,
    pub script_released: bool,
    pub descriptor: ContainmentDescriptor,
    pub containment_empty: bool,
    pub background_descendants: bool,
    pub output_capture: OutputCaptureSummary,
}

#[derive(Debug, Clone)]
pub struct ScriptRecoveryRequest {
    pub token: String,
    pub runtime_directory: PathBuf,
    pub descriptor: ContainmentDescriptor,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptRecoveryOutcome {
    /// A validated supervisor result proves the invocation never crossed the
    /// exec gate and the backend-owned domain is empty.
    ExecutionNeverReleased,
    ContainmentAlreadyEmpty,
    ContainmentTerminated,
    ManualRecoveryRequired(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SupervisorSpec {
    schema_version: u32,
    token: String,
    runtime_identity: RuntimeDirectoryIdentity,
    #[serde(default)]
    containment_preference: ContainmentPreference,
    script: PathBuf,
    args: Vec<String>,
    working_directory: PathBuf,
    environment: BTreeMap<String, String>,
    timeout_millis: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct TerminationSummary {
    reason: TerminationReason,
    graceful_deadline_unix_millis: u64,
    forced: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SupervisorResult {
    schema_version: u32,
    token: String,
    raw_wait_status: Option<i32>,
    timed_out: bool,
    cancelled: bool,
    script_released: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    termination: Option<TerminationSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    descriptor: Option<ContainmentDescriptor>,
    containment_empty: bool,
    background_descendants: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    internal_error: Option<String>,
}

impl SupervisorResult {
    fn internal(token: String, error: impl Into<String>) -> Self {
        Self {
            schema_version: SUPERVISOR_RESULT_SCHEMA,
            token,
            raw_wait_status: None,
            timed_out: false,
            cancelled: false,
            script_released: false,
            termination: None,
            descriptor: None,
            containment_empty: false,
            background_descendants: false,
            internal_error: Some(error.into()),
        }
    }
}

fn validate_supervisor_result(
    result: &SupervisorResult,
    expected_token: &str,
) -> Result<(), ScriptSupervisorError> {
    if result.schema_version != SUPERVISOR_RESULT_SCHEMA || result.token != expected_token {
        return Err(ScriptSupervisorError::Protocol(
            "script supervisor result schema/token mismatch".to_string(),
        ));
    }
    if result.internal_error.is_some() {
        if result.raw_wait_status.is_some()
            || result.timed_out
            || result.cancelled
            || result.script_released
            || result.termination.is_some()
            || result.descriptor.is_some()
            || result.containment_empty
            || result.background_descendants
        {
            return Err(ScriptSupervisorError::Protocol(
                "internal-error supervisor result contains contradictory execution progress"
                    .to_string(),
            ));
        }
        return Ok(());
    }
    if result.descriptor.is_none() || result.raw_wait_status.is_none() {
        return Err(ScriptSupervisorError::Protocol(
            "terminal script supervisor result omitted its descriptor or leader status"
                .to_string(),
        ));
    }
    if !result.containment_empty {
        return Err(ScriptSupervisorError::Protocol(
            "terminal script supervisor result did not prove containment empty".to_string(),
        ));
    }
    match result.termination.as_ref().map(|entry| entry.reason) {
        Some(TerminationReason::Timeout) if !result.timed_out => {
            return Err(ScriptSupervisorError::Protocol(
                "timeout termination record lacks the timed_out result flag".to_string(),
            ));
        }
        Some(TerminationReason::Cancellation) | Some(TerminationReason::ParentDisconnected)
            if !result.cancelled =>
        {
            return Err(ScriptSupervisorError::Protocol(
                "cancellation termination record lacks the cancelled result flag".to_string(),
            ));
        }
        Some(TerminationReason::LeaderExitedWithDescendants)
            if !result.background_descendants =>
        {
            return Err(ScriptSupervisorError::Protocol(
                "background-descendant termination record lacks its result flag".to_string(),
            ));
        }
        Some(TerminationReason::Recovery) => {
            return Err(ScriptSupervisorError::Protocol(
                "live supervisor result cannot claim restart-recovery termination".to_string(),
            ));
        }
        _ => {}
    }
    if result.timed_out
        && !matches!(
            result.termination.as_ref().map(|entry| entry.reason),
            Some(TerminationReason::Timeout)
        )
    {
        return Err(ScriptSupervisorError::Protocol(
            "timed_out result lacks a matching termination record".to_string(),
        ));
    }
    if result.background_descendants
        && !matches!(
            result.termination.as_ref().map(|entry| entry.reason),
            Some(TerminationReason::LeaderExitedWithDescendants)
        )
    {
        return Err(ScriptSupervisorError::Protocol(
            "background-descendant result lacks a matching termination record".to_string(),
        ));
    }
    if result.cancelled
        && !matches!(
            result.termination.as_ref().map(|entry| entry.reason),
            Some(TerminationReason::Cancellation) | Some(TerminationReason::ParentDisconnected)
        )
    {
        return Err(ScriptSupervisorError::Protocol(
            "cancelled result lacks a matching termination record".to_string(),
        ));
    }
    if !result.script_released && result.termination.is_none() {
        return Err(ScriptSupervisorError::Protocol(
            "exec-gated terminal result omitted its termination record".to_string(),
        ));
    }
    Ok(())
}

fn cargo_test_helper_candidate(current_executable: &Path) -> Option<PathBuf> {
    let deps = current_executable.parent()?;
    if deps.file_name().and_then(|value| value.to_str()) != Some("deps") {
        return None;
    }
    let profile_dir = deps.parent()?;
    if !profile_dir.join(".fingerprint").is_dir() {
        return None;
    }
    let stem = current_executable.file_stem()?.to_str()?;
    let (_, hash) = stem.rsplit_once('-')?;
    if hash.len() < 8 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    Some(profile_dir.join(format!(
        "tonepoet{}",
        std::env::consts::EXE_SUFFIX
    )))
}

fn resolve_supervisor_helper_executable(
    explicit: Option<&Path>,
) -> Result<PathBuf, ScriptSupervisorError> {
    if let Some(path) = explicit {
        return Ok(path.to_path_buf());
    }
    if let Some(path) = std::env::var_os("TONEPOET_SCRIPT_SUPERVISOR_HELPER") {
        return Ok(PathBuf::from(path));
    }

    let current = crate::reexec::current_executable_for_reexec().map_err(|error| {
        ScriptSupervisorError::Internal(format!(
            "cannot locate the current executable for script supervision: {error}"
        ))
    })?;
    let Some(default_test_helper) = cargo_test_helper_candidate(&current) else {
        // Production behavior remains re-exec of the running Tonepoet image.
        return Ok(current);
    };

    // Cargo may expose the real binary explicitly for integration tests. Fall
    // back to the standard target/{profile}/tonepoet sibling used by
    // `cargo build && cargo test --lib` and by workspace test builds.
    if let Some(path) = std::env::var_os("CARGO_BIN_EXE_tonepoet") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
    }
    if default_test_helper.is_file() {
        return Ok(default_test_helper);
    }

    Err(ScriptSupervisorError::Internal(format!(
        "cargo test harness cannot re-exec the supervisor entrypoint; expected built tonepoet helper at {}",
        default_test_helper.display()
    )))
}

/// Run a script through the dedicated supervisor helper.
///
/// `is_cancelled` is polled by the parent.  The helper independently enforces
/// the timeout, so a stalled conversion worker cannot silently disable it.
pub fn run_supervised<F, E>(
    invocation: &SupervisedCommand,
    is_cancelled: F,
    mut on_event: E,
) -> Result<SupervisedOutcome, ScriptSupervisorError>
where
    F: Fn() -> bool,
    E: FnMut(&ScriptLifecycleEvent) -> Result<(), ScriptSupervisorError>,
{
    if !valid_token(&invocation.token) {
        return Err(ScriptSupervisorError::Protocol(
            "invalid script containment token".to_string(),
        ));
    }
    let runtime_directory = open_private_runtime_directory(
        &invocation.runtime_directory,
        invocation.runtime_identity,
    )?;
    let runtime_fd = runtime_directory.as_raw_fd();
    let spec = SupervisorSpec {
        schema_version: SUPERVISOR_RESULT_SCHEMA,
        token: invocation.token.clone(),
        runtime_identity: invocation.runtime_identity,
        containment_preference: invocation.containment_preference,
        script: invocation.script.clone(),
        args: invocation.args.clone(),
        working_directory: invocation.working_directory.clone(),
        environment: invocation.environment.clone(),
        timeout_millis: duration_millis_u64(invocation.timeout),
    };
    write_private_json_new_at(runtime_fd, SPEC_FILE_NAME, &spec)?;
    sync_directory(runtime_fd)?;

    let (mut control_parent, control_child) = UnixStream::pair()?;
    let control_fd = control_child.as_raw_fd();
    let (mut event_parent, event_child) = UnixStream::pair()?;
    let event_fd = event_child.as_raw_fd();
    set_nonblocking(event_parent.as_raw_fd())?;

    let script_fd = invocation.script_file.as_raw_fd();
    let script_metadata = invocation.script_file.metadata()?;
    if !script_metadata.is_file() {
        return Err(ScriptSupervisorError::Protocol(
            "retained reviewed executable descriptor is not a regular file".to_string(),
        ));
    }
    let working_directory_fd = invocation.working_directory_file.as_raw_fd();
    let working_directory_metadata = invocation.working_directory_file.metadata()?;
    if !working_directory_metadata.is_dir() {
        return Err(ScriptSupervisorError::Protocol(
            "retained working-directory descriptor is not a directory".to_string(),
        ));
    }

    let helper_executable =
        resolve_supervisor_helper_executable(invocation.helper_executable.as_deref())?;
    let mut command = Command::new(helper_executable);
    command
        .arg(INTERNAL_SUBCOMMAND)
        .arg("--runtime-fd")
        .arg(runtime_fd.to_string())
        .arg("--control-fd")
        .arg(control_fd.to_string())
        .arg("--event-fd")
        .arg(event_fd.to_string())
        .arg("--script-fd")
        .arg(script_fd.to_string())
        .arg("--working-directory-fd")
        .arg(working_directory_fd.to_string());
    let retained_lifetime_fds: Vec<RawFd> = invocation
        .retained_lifetime_files
        .iter()
        .map(|file| file.as_raw_fd())
        .collect();
    for fd in &retained_lifetime_fds {
        command.arg("--retained-lifetime-fd").arg(fd.to_string());
    }
    command.env_clear();
    if let Some(file) = invocation.stdin_file.as_ref() {
        command.stdin(Stdio::from(file.try_clone()?));
    } else {
        command.stdin(Stdio::null());
    }
    if let Some(file) = invocation.stdout_file.as_ref() {
        command.stdout(Stdio::from(file.try_clone()?));
    } else {
        command.stdout(Stdio::piped());
    }
    if let Some(file) = invocation.stderr_file.as_ref() {
        command.stderr(Stdio::from(file.try_clone()?));
    } else {
        command.stderr(Stdio::piped());
    }
    unsafe {
        command.pre_exec(move || {
            clear_close_on_exec(runtime_fd)?;
            clear_close_on_exec(control_fd)?;
            clear_close_on_exec(event_fd)?;
            clear_close_on_exec(script_fd)?;
            clear_close_on_exec(working_directory_fd)?;
            for fd in &retained_lifetime_fds {
                clear_close_on_exec(*fd)?;
            }
            Ok(())
        });
    }
    let mut helper = command.spawn()?;
    drop(control_child);
    drop(event_child);

    let stderr = helper.stderr.take();
    let output_stop = Arc::new(AtomicBool::new(false));
    let stdout_reader = match helper.stdout.take() {
        Some(stdout) => Some(match spawn_tail_reader(stdout, Arc::clone(&output_stop)) {
            Ok(reader) => reader,
            Err(error) => {
                let _ = send_control(&mut control_parent, CONTROL_CANCEL);
                let _ = helper.wait();
                return Err(error);
            }
        }),
        None if invocation.stdout_file.is_some() => None,
        None => {
            let _ = send_control(&mut control_parent, CONTROL_CANCEL);
            let _ = helper.wait();
            return Err(ScriptSupervisorError::Protocol("supervisor stdout pipe is unavailable".to_string()));
        }
    };
    let stderr_reader = match stderr {
        Some(stderr) => Some(match spawn_tail_reader(stderr, Arc::clone(&output_stop)) {
            Ok(reader) => reader,
            Err(error) => {
                let _ = send_control(&mut control_parent, CONTROL_CANCEL);
                let _ = helper.wait();
                output_stop.store(true, Ordering::Release);
                if let Some(reader) = stdout_reader { let _ = join_tail_reader(reader, "stdout"); }
                return Err(error);
            }
        }),
        None if invocation.stderr_file.is_some() => None,
        None => {
            let _ = send_control(&mut control_parent, CONTROL_CANCEL);
            let _ = helper.wait();
            output_stop.store(true, Ordering::Release);
            let _ = join_optional_tail_reader(stdout_reader, "stdout");
            return Err(ScriptSupervisorError::Protocol("supervisor stderr pipe is unavailable".to_string()));
        }
    };

    // `invocation.timeout` is a user-code runtime budget, not a supervisor
    // setup budget. The containment backends already start the same timeout
    // after releasing the reviewed script. Mirror that boundary in the parent
    // so full-system load cannot turn slow containment setup into a timeout
    // that kills the launcher before user code has ever run.
    let mut user_code_started: Option<Instant> = None;
    let mut control_sent = false;
    let mut event_reader = EventFrameReader::default();
    let mut observed_descriptor: Option<ContainmentDescriptor> = None;
    let helper_status = loop {
        for event in event_reader.read_available(&mut event_parent)? {
            if event.schema_version() != LIFECYCLE_EVENT_SCHEMA {
                let _ = event_parent.write_all(&[EVENT_ABORT]);
                let _ = send_control(&mut control_parent, CONTROL_CANCEL);
                let _ = helper.wait();
                output_stop.store(true, Ordering::Release);
                let _ = join_optional_tail_reader(stdout_reader, "stdout");
                let _ = join_optional_tail_reader(stderr_reader, "stderr");
                return Err(ScriptSupervisorError::Protocol(
                    "script supervisor emitted an unsupported lifecycle event".to_string(),
                ));
            }
            if let ScriptLifecycleEvent::ContainmentPrepared { descriptor, .. } = &event {
                validate_descriptor(descriptor, &invocation.token)?;
                if observed_descriptor.replace(descriptor.clone()).is_some() {
                    let _ = event_parent.write_all(&[EVENT_ABORT]);
                    return Err(ScriptSupervisorError::Protocol(
                        "script supervisor emitted multiple containment descriptors".to_string(),
                    ));
                }
            }
            match on_event(&event) {
                Ok(()) => {
                    // Containment preparation is the final exec gate. Persist
                    // it first, then queue cancellation before ACK so the
                    // helper cannot release user code in the cancellation race
                    // between callback completion and the next poll. Timeout
                    // deliberately starts only after UserCodeReleased.
                    if matches!(event, ScriptLifecycleEvent::ContainmentPrepared { .. })
                        && !control_sent
                        && is_cancelled()
                    {
                        send_control(&mut control_parent, CONTROL_CANCEL)?;
                        control_sent = true;
                    }
                    if matches!(event, ScriptLifecycleEvent::UserCodeReleased { .. }) {
                        user_code_started.get_or_insert_with(Instant::now);
                    }
                    event_parent.write_all(&[EVENT_ACK])?;
                }
                Err(error) => {
                    let _ = event_parent.write_all(&[EVENT_ABORT]);
                    let _ = send_control(&mut control_parent, CONTROL_CANCEL);
                    let _ = helper.wait();
                    output_stop.store(true, Ordering::Release);
                    let _ = join_optional_tail_reader(stdout_reader, "stdout");
                    let _ = join_optional_tail_reader(stderr_reader, "stderr");
                    return Err(error);
                }
            }
        }
        if let Some(status) = helper.try_wait()? {
            break status;
        }
        if !control_sent && is_cancelled() {
            send_control(&mut control_parent, CONTROL_CANCEL)?;
            control_sent = true;
        } else if !control_sent
            && user_code_started
                .is_some_and(|started| started.elapsed() >= invocation.timeout)
        {
            send_control(&mut control_parent, CONTROL_TIMEOUT)?;
            control_sent = true;
        }
        thread::sleep(Duration::from_millis(10));
    };
    drop(control_parent);

    let drain_deadline = Instant::now() + TAIL_DRAIN_GRACE;
    while Instant::now() < drain_deadline {
        let events = event_reader.read_available(&mut event_parent)?;
        if events.is_empty() {
            thread::sleep(Duration::from_millis(5));
            continue;
        }
        for event in events {
            if event.schema_version() != LIFECYCLE_EVENT_SCHEMA {
                return Err(ScriptSupervisorError::Protocol(
                    "script supervisor emitted an unsupported lifecycle event".to_string(),
                ));
            }
            on_event(&event)?;
            let _ = event_parent.write_all(&[EVENT_ACK]);
        }
    }

    // Do not let an unobservable platform escape keep inherited output pipes
    // open forever after the authenticated supervisor has terminated.
    output_stop.store(true, Ordering::Release);
    let stdout_capture = join_optional_tail_reader(stdout_reader, "stdout")?;
    let stderr_capture = join_optional_tail_reader(stderr_reader, "stderr")?;
    let output_capture = OutputCaptureSummary {
        stdout: stdout_capture.terminal,
        stderr: stderr_capture.terminal,
    };
    on_event(&ScriptLifecycleEvent::OutputCaptureCompleted {
        schema_version: LIFECYCLE_EVENT_SCHEMA,
        summary: output_capture.clone(),
    })?;

    if !helper_status.success() && !entry_exists_no_follow_at(runtime_fd, RESULT_FILE_NAME)? {
        return Err(ScriptSupervisorError::Internal(format!(
            "script supervisor exited as {helper_status} without a result record"
        )));
    }
    let result: SupervisorResult = read_json_no_follow_at(runtime_fd, RESULT_FILE_NAME)?;
    validate_supervisor_result(&result, &invocation.token)?;
    if let Some(error) = result.internal_error {
        return Err(ScriptSupervisorError::Internal(error));
    }
    let descriptor = result.descriptor.ok_or_else(|| {
        ScriptSupervisorError::Protocol(
            "script supervisor result omitted the containment descriptor".to_string(),
        )
    })?;
    validate_descriptor(&descriptor, &invocation.token)?;
    if let Some(observed) = observed_descriptor {
        if observed != descriptor {
            return Err(ScriptSupervisorError::Protocol(
                "script supervisor result changed the prepared containment identity".to_string(),
            ));
        }
    }
    let raw_wait_status = result.raw_wait_status.ok_or_else(|| {
        ScriptSupervisorError::Protocol(
            "script supervisor result omitted the script wait status".to_string(),
        )
    })?;
    Ok(SupervisedOutcome {
        status: ExitStatus::from_raw(raw_wait_status),
        stdout_tail: stdout_capture.bytes,
        stderr_tail: stderr_capture.bytes,
        timed_out: result.timed_out,
        cancelled: result.cancelled,
        script_released: result.script_released,
        descriptor,
        containment_empty: result.containment_empty,
        background_descendants: result.background_descendants,
        output_capture,
    })
}


/// Long-lived tonepoet-owned execution supervisor for one active queue item.
/// The dedicated process is the only cross-command holder of QueueExecution,
/// ExecutionClaim and ExecutionStaging lease duplicates. Individual contained
/// commands run in forked backend workers inside this supervisor process; those
/// workers never become ownership anchors and third-party programs never
/// receive the coordination descriptors.
#[derive(Clone)]
pub struct ItemExecutionSupervisorClient {
    request: Arc<Mutex<UnixStream>>,
    child: Arc<Mutex<Option<Child>>>,
    supervisor_pid: u32,
}

impl std::fmt::Debug for ItemExecutionSupervisorClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ItemExecutionSupervisorClient").finish_non_exhaustive()
    }
}

impl ItemExecutionSupervisorClient {
    pub fn start(initial_lifetime_files: &[Arc<File>]) -> Result<Self, ScriptSupervisorError> {
        let (parent, child_stream) = UnixStream::pair()?;
        let request_fd = child_stream.as_raw_fd();
        let helper = resolve_supervisor_helper_executable(None)?;
        let retained_fds = initial_lifetime_files
            .iter()
            .map(|file| file.as_raw_fd())
            .collect::<Vec<_>>();
        let mut command = Command::new(helper);
        command
            .arg(INTERNAL_ITEM_SUPERVISOR_SUBCOMMAND)
            .arg("--request-fd")
            .arg(request_fd.to_string())
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        for fd in &retained_fds {
            command.arg("--retained-lifetime-fd").arg(fd.to_string());
        }
        unsafe {
            command.pre_exec(move || {
                clear_close_on_exec(request_fd)?;
                for fd in &retained_fds {
                    clear_close_on_exec(*fd)?;
                }
                Ok(())
            });
        }
        let child = command.spawn()?;
        let supervisor_pid = child.id();
        drop(child_stream);
        Ok(Self {
            request: Arc::new(Mutex::new(parent)),
            child: Arc::new(Mutex::new(Some(child))),
            supervisor_pid,
        })
    }

    pub fn process_id(&self) -> u32 {
        self.supervisor_pid
    }

    /// Transfer one later-acquired execution/path/staging lease to the item
    /// supervisor and wait for its acknowledgement. The caller must not release
    /// a dependent external command before this returns successfully.
    pub fn handoff_lifetime_file(&self, file: &File) -> Result<(), ScriptSupervisorError> {
        let mut request = self
            .request
            .lock()
            .map_err(|_| ScriptSupervisorError::Internal("item supervisor request lock poisoned".to_string()))?;
        send_item_request(&request, ITEM_REQUEST_LEASE, &[file.as_raw_fd()])?;
        read_item_ack(&mut request)
    }

    fn submit_run(&self, fds: &[RawFd]) -> Result<(), ScriptSupervisorError> {
        if fds.len() != ITEM_MAX_FDS {
            return Err(ScriptSupervisorError::Protocol(format!(
                "item supervisor run request supplied {} descriptors; expected {ITEM_MAX_FDS}",
                fds.len()
            )));
        }
        let mut request = self
            .request
            .lock()
            .map_err(|_| ScriptSupervisorError::Internal("item supervisor request lock poisoned".to_string()))?;
        send_item_request(&request, ITEM_REQUEST_RUN, fds)?;
        read_item_ack(&mut request)
    }

    pub fn shutdown(&self) -> Result<(), ScriptSupervisorError> {
        {
            let mut request = self
                .request
                .lock()
                .map_err(|_| ScriptSupervisorError::Internal("item supervisor request lock poisoned".to_string()))?;
            send_item_request(&request, ITEM_REQUEST_SHUTDOWN, &[])?;
            read_item_ack(&mut request)?;
        }
        if let Some(mut child) = self
            .child
            .lock()
            .map_err(|_| ScriptSupervisorError::Internal("item supervisor child lock poisoned".to_string()))?
            .take()
        {
            let status = child.wait()?;
            if !status.success() {
                return Err(ScriptSupervisorError::Internal(format!(
                    "item execution supervisor exited as {status}"
                )));
            }
        }
        Ok(())
    }
}

fn read_item_ack(stream: &mut UnixStream) -> Result<(), ScriptSupervisorError> {
    let mut ack = [0_u8; 1];
    stream.read_exact(&mut ack)?;
    if ack[0] != ITEM_REQUEST_ACK {
        return Err(ScriptSupervisorError::Protocol(
            "item execution supervisor returned an invalid acknowledgement".to_string(),
        ));
    }
    Ok(())
}

fn send_item_request(stream: &UnixStream, tag: u8, fds: &[RawFd]) -> io::Result<()> {
    if fds.len() > ITEM_MAX_FDS {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "too many item-supervisor descriptors"));
    }
    let mut byte = [tag];
    let mut iov = libc::iovec {
        iov_base: byte.as_mut_ptr().cast(),
        iov_len: 1,
    };
    let control_len = if fds.is_empty() {
        0
    } else {
        unsafe { libc::CMSG_SPACE((fds.len() * std::mem::size_of::<RawFd>()) as _) as usize }
    };
    let mut control = vec![0_u8; control_len];
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    if !fds.is_empty() {
        msg.msg_control = control.as_mut_ptr().cast();
        msg.msg_controllen = control.len();
        unsafe {
            let cmsg = libc::CMSG_FIRSTHDR(&msg);
            if cmsg.is_null() {
                return Err(io::Error::new(io::ErrorKind::Other, "cannot allocate SCM_RIGHTS header"));
            }
            (*cmsg).cmsg_level = libc::SOL_SOCKET;
            (*cmsg).cmsg_type = libc::SCM_RIGHTS;
            (*cmsg).cmsg_len = libc::CMSG_LEN((fds.len() * std::mem::size_of::<RawFd>()) as _) as usize;
            std::ptr::copy_nonoverlapping(
                fds.as_ptr().cast::<u8>(),
                libc::CMSG_DATA(cmsg),
                fds.len() * std::mem::size_of::<RawFd>(),
            );
        }
    }
    let sent = unsafe { libc::sendmsg(stream.as_raw_fd(), &msg, 0) };
    if sent == 1 { Ok(()) } else if sent < 0 { Err(io::Error::last_os_error()) } else {
        Err(io::Error::new(io::ErrorKind::WriteZero, "short item-supervisor request write"))
    }
}

fn receive_item_request(stream: &UnixStream) -> io::Result<Option<(u8, Vec<File>)>> {
    let mut tag = [0_u8; 1];
    let mut iov = libc::iovec {
        iov_base: tag.as_mut_ptr().cast(),
        iov_len: 1,
    };
    let control_len = unsafe {
        libc::CMSG_SPACE((ITEM_MAX_FDS * std::mem::size_of::<RawFd>()) as _) as usize
    };
    let mut control = vec![0_u8; control_len];
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.as_mut_ptr().cast();
    msg.msg_controllen = control.len();
    let received = unsafe { libc::recvmsg(stream.as_raw_fd(), &mut msg, 0) };
    if received == 0 {
        return Ok(None);
    }
    if received < 0 {
        return Err(io::Error::last_os_error());
    }
    if received != 1 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid item-supervisor request frame"));
    }
    let mut files = Vec::new();
    unsafe {
        let mut cmsg = libc::CMSG_FIRSTHDR(&msg);
        while !cmsg.is_null() {
            if (*cmsg).cmsg_level == libc::SOL_SOCKET && (*cmsg).cmsg_type == libc::SCM_RIGHTS {
                let base = libc::CMSG_LEN(0) as usize;
                let bytes = (*cmsg).cmsg_len.saturating_sub(base);
                if bytes % std::mem::size_of::<RawFd>() != 0 {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, "misaligned SCM_RIGHTS payload"));
                }
                let count = bytes / std::mem::size_of::<RawFd>();
                let data = libc::CMSG_DATA(cmsg).cast::<RawFd>();
                for index in 0..count {
                    let fd = *data.add(index);
                    set_close_on_exec(fd)?;
                    files.push(File::from_raw_fd(fd));
                }
            }
            cmsg = libc::CMSG_NXTHDR(&msg, cmsg);
        }
    }
    Ok(Some((tag[0], files)))
}

fn create_pipe_files() -> io::Result<(File, File)> {
    let mut fds = [-1_i32; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if let Err(error) = set_close_on_exec(fds[0]).and_then(|_| set_close_on_exec(fds[1])) {
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
        return Err(error);
    }
    unsafe { Ok((File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1]))) }
}

/// Hidden long-lived per-item supervisor entry point. It stays single-threaded
/// so a per-command `fork` can safely create an isolated containment backend
/// worker with command-specific cwd/stdin/stdout/stderr while this parent keeps
/// all lifetime lease descriptors. No third-party process receives those fds.
pub fn run_internal_execution_item_supervisor(
    request_fd: RawFd,
    retained_lifetime_fds: &[RawFd],
) -> Result<(), ScriptSupervisorError> {
    if request_fd < 0 {
        return Err(ScriptSupervisorError::Protocol("invalid item supervisor request descriptor".to_string()));
    }
    let request = unsafe { UnixStream::from_raw_fd(request_fd) };
    set_close_on_exec(request.as_raw_fd())?;
    let mut lifetime_files = Vec::with_capacity(retained_lifetime_fds.len());
    for fd in retained_lifetime_fds {
        if *fd < 0 {
            return Err(ScriptSupervisorError::Protocol("invalid item supervisor lifetime descriptor".to_string()));
        }
        let file = unsafe { File::from_raw_fd(*fd) };
        set_close_on_exec(file.as_raw_fd())?;
        lifetime_files.push(file);
    }
    let mut workers = BTreeSet::<libc::pid_t>::new();
    let mut shutting_down = false;
    loop {
        reap_item_workers(&mut workers)?;
        if shutting_down {
            if workers.is_empty() {
                return Ok(());
            }
            thread::sleep(CONTROL_POLL_INTERVAL);
            continue;
        }
        let mut pollfd = libc::pollfd {
            fd: request.as_raw_fd(),
            events: libc::POLLIN | libc::POLLHUP | libc::POLLERR,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&mut pollfd, 1, 50) };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted { continue; }
            return Err(error.into());
        }
        if ready == 0 { continue; }
        let request_frame = receive_item_request(&request)?;
        let Some((tag, mut files)) = request_frame else {
            shutting_down = true;
            continue;
        };
        match tag {
            ITEM_REQUEST_LEASE => {
                if files.len() != 1 {
                    return Err(ScriptSupervisorError::Protocol("lease handoff must contain exactly one fd".to_string()));
                }
                lifetime_files.push(files.remove(0));
                (&request).write_all(&[ITEM_REQUEST_ACK])?;
            }
            ITEM_REQUEST_RUN => {
                if files.len() != ITEM_MAX_FDS {
                    return Err(ScriptSupervisorError::Protocol(format!(
                        "run handoff contained {} fds; expected {ITEM_MAX_FDS}", files.len()
                    )));
                }
                let pid = unsafe { libc::fork() };
                if pid < 0 {
                    return Err(io::Error::last_os_error().into());
                }
                if pid == 0 {
                    // The item supervisor, not this backend worker, owns the
                    // persistent lifetime descriptors. Close inherited copies
                    // before any launcher or target process is created.
                    unsafe { libc::close(request.as_raw_fd()); }
                    for file in &lifetime_files {
                        unsafe { libc::close(file.as_raw_fd()); }
                    }
                    let raw = files.iter().map(|file| file.as_raw_fd()).collect::<Vec<_>>();
                    // `run_internal_supervisor` takes ownership of several raw
                    // descriptors with `File::from_raw_fd`. The fork child exits
                    // via `_exit`, but forgetting this container also makes the
                    // single-owner intent explicit and avoids duplicate File
                    // ownership during backend execution.
                    std::mem::forget(files);
                    let status = run_item_backend_worker(&raw);
                    unsafe { libc::_exit(if status.is_ok() { 0 } else { 70 }); }
                }
                workers.insert(pid);
                drop(files);
                (&request).write_all(&[ITEM_REQUEST_ACK])?;
            }
            ITEM_REQUEST_SHUTDOWN => {
                if !files.is_empty() {
                    return Err(ScriptSupervisorError::Protocol("shutdown request carried unexpected fds".to_string()));
                }
                (&request).write_all(&[ITEM_REQUEST_ACK])?;
                shutting_down = true;
            }
            _ => return Err(ScriptSupervisorError::Protocol("unknown item supervisor request".to_string())),
        }
    }
}

fn run_item_backend_worker(raw: &[RawFd]) -> Result<(), ScriptSupervisorError> {
    if raw.len() != ITEM_MAX_FDS {
        return Err(ScriptSupervisorError::Protocol("invalid backend worker fd set".to_string()));
    }
    for (source, target) in [(raw[5], 0), (raw[6], 1), (raw[7], 2)] {
        if source != target && unsafe { libc::dup2(source, target) } < 0 {
            return Err(io::Error::last_os_error().into());
        }
        clear_close_on_exec(target)?;
    }
    run_internal_supervisor(raw[0], raw[1], raw[2], raw[3], raw[4], &[])
}

fn reap_item_workers(workers: &mut BTreeSet<libc::pid_t>) -> io::Result<()> {
    let pids = workers.iter().copied().collect::<Vec<_>>();
    for pid in pids {
        let mut status = 0_i32;
        let result = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        if result == pid || (result < 0 && io::Error::last_os_error().raw_os_error() == Some(libc::ECHILD)) {
            workers.remove(&pid);
        } else if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted { return Err(error); }
        }
    }
    Ok(())
}

/// Run one contained command through an already-live item supervisor. The
/// parent continues to own lifecycle persistence/exec-gate acknowledgement;
/// only the process creation moves behind the persistent item supervisor.
pub fn run_supervised_via_item_supervisor<F, E>(
    invocation: &SupervisedCommand,
    item_supervisor: &ItemExecutionSupervisorClient,
    is_cancelled: F,
    mut on_event: E,
) -> Result<SupervisedOutcome, ScriptSupervisorError>
where
    F: Fn() -> bool,
    E: FnMut(&ScriptLifecycleEvent) -> Result<(), ScriptSupervisorError>,
{
    if !valid_token(&invocation.token) {
        return Err(ScriptSupervisorError::Protocol("invalid script containment token".to_string()));
    }
    let runtime_directory = open_private_runtime_directory(
        &invocation.runtime_directory,
        invocation.runtime_identity,
    )?;
    let runtime_fd = runtime_directory.as_raw_fd();
    let spec = SupervisorSpec {
        schema_version: SUPERVISOR_RESULT_SCHEMA,
        token: invocation.token.clone(),
        runtime_identity: invocation.runtime_identity,
        containment_preference: invocation.containment_preference,
        script: invocation.script.clone(),
        args: invocation.args.clone(),
        working_directory: invocation.working_directory.clone(),
        environment: invocation.environment.clone(),
        timeout_millis: duration_millis_u64(invocation.timeout),
    };
    write_private_json_new_at(runtime_fd, SPEC_FILE_NAME, &spec)?;
    sync_directory(runtime_fd)?;

    let (mut control_parent, control_child) = UnixStream::pair()?;
    let (mut event_parent, event_child) = UnixStream::pair()?;
    set_nonblocking(event_parent.as_raw_fd())?;

    if !invocation.script_file.metadata()?.is_file() {
        return Err(ScriptSupervisorError::Protocol("retained reviewed executable descriptor is not a regular file".to_string()));
    }
    if !invocation.working_directory_file.metadata()?.is_dir() {
        return Err(ScriptSupervisorError::Protocol("retained working-directory descriptor is not a directory".to_string()));
    }

    let stdin_owned = match invocation.stdin_file.as_ref() {
        Some(file) => file.try_clone()?,
        None => OpenOptions::new().read(true).open("/dev/null")?,
    };
    let (stdout_reader_file, stdout_send) = match invocation.stdout_file.as_ref() {
        Some(file) => (None, file.try_clone()?),
        None => {
            let (read, write) = create_pipe_files()?;
            (Some(read), write)
        }
    };
    let (stderr_reader_file, stderr_send) = match invocation.stderr_file.as_ref() {
        Some(file) => (None, file.try_clone()?),
        None => {
            let (read, write) = create_pipe_files()?;
            (Some(read), write)
        }
    };
    let run_fds = [
        runtime_fd,
        control_child.as_raw_fd(),
        event_child.as_raw_fd(),
        invocation.script_file.as_raw_fd(),
        invocation.working_directory_file.as_raw_fd(),
        stdin_owned.as_raw_fd(),
        stdout_send.as_raw_fd(),
        stderr_send.as_raw_fd(),
    ];
    item_supervisor.submit_run(&run_fds)?;
    drop(control_child);
    drop(event_child);
    drop(stdin_owned);
    drop(stdout_send);
    drop(stderr_send);

    let output_stop = Arc::new(AtomicBool::new(false));
    let stdout_reader = match stdout_reader_file {
        Some(reader) => Some(spawn_tail_reader(reader, Arc::clone(&output_stop))?),
        None => None,
    };
    let stderr_reader = match stderr_reader_file {
        Some(reader) => Some(spawn_tail_reader(reader, Arc::clone(&output_stop))?),
        None => None,
    };

    let supervisor_started = Instant::now();
    let mut user_code_started: Option<Instant> = None;
    let terminal_grace = TERM_GRACE + KILL_GRACE + Duration::from_secs(5);
    let mut control_sent = false;
    let mut event_reader = EventFrameReader::default();
    let mut observed_descriptor: Option<ContainmentDescriptor> = None;
    loop {
        for event in event_reader.read_available(&mut event_parent)? {
            if event.schema_version() != LIFECYCLE_EVENT_SCHEMA {
                let _ = event_parent.write_all(&[EVENT_ABORT]);
                let _ = send_control(&mut control_parent, CONTROL_CANCEL);
                return Err(ScriptSupervisorError::Protocol("item supervisor emitted an unsupported lifecycle event".to_string()));
            }
            if let ScriptLifecycleEvent::ContainmentPrepared { descriptor, .. } = &event {
                validate_descriptor(descriptor, &invocation.token)?;
                if observed_descriptor.replace(descriptor.clone()).is_some() {
                    let _ = event_parent.write_all(&[EVENT_ABORT]);
                    return Err(ScriptSupervisorError::Protocol("item supervisor emitted multiple containment descriptors".to_string()));
                }
            }
            match on_event(&event) {
                Ok(()) => {
                    if matches!(event, ScriptLifecycleEvent::ContainmentPrepared { .. })
                        && !control_sent
                        && is_cancelled()
                    {
                        send_control(&mut control_parent, CONTROL_CANCEL)?;
                        control_sent = true;
                    }
                    if matches!(event, ScriptLifecycleEvent::UserCodeReleased { .. }) {
                        user_code_started.get_or_insert_with(Instant::now);
                    }
                    event_parent.write_all(&[EVENT_ACK])?;
                }
                Err(error) => {
                    let _ = event_parent.write_all(&[EVENT_ABORT]);
                    let _ = send_control(&mut control_parent, CONTROL_CANCEL);
                    return Err(error);
                }
            }
        }
        if entry_exists_no_follow_at(runtime_fd, RESULT_FILE_NAME)? {
            break;
        }
        if !control_sent && is_cancelled() {
            send_control(&mut control_parent, CONTROL_CANCEL)?;
            control_sent = true;
        } else if !control_sent
            && user_code_started
                .is_some_and(|started| started.elapsed() >= invocation.timeout)
        {
            send_control(&mut control_parent, CONTROL_TIMEOUT)?;
            control_sent = true;
        }
        let terminal_window_exhausted = match user_code_started {
            Some(started) => started.elapsed() >= invocation.timeout + terminal_grace,
            // A helper that never reaches UserCodeReleased still gets a hard
            // protocol bound; this is intentionally independent of the
            // command's runtime timeout.
            None => supervisor_started.elapsed() >= LIFECYCLE_IO_TIMEOUT,
        };
        if terminal_window_exhausted {
            output_stop.store(true, Ordering::Release);
            return Err(ScriptSupervisorError::Internal(
                "item supervisor backend did not publish a terminal containment result within the bounded recovery window".to_string(),
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }
    drop(control_parent);

    let drain_deadline = Instant::now() + TAIL_DRAIN_GRACE;
    while Instant::now() < drain_deadline {
        let events = event_reader.read_available(&mut event_parent)?;
        if events.is_empty() {
            thread::sleep(Duration::from_millis(5));
            continue;
        }
        for event in events {
            if event.schema_version() != LIFECYCLE_EVENT_SCHEMA {
                return Err(ScriptSupervisorError::Protocol("item supervisor emitted an unsupported lifecycle event".to_string()));
            }
            on_event(&event)?;
            let _ = event_parent.write_all(&[EVENT_ACK]);
        }
    }

    output_stop.store(true, Ordering::Release);
    let stdout_capture = join_optional_tail_reader(stdout_reader, "stdout")?;
    let stderr_capture = join_optional_tail_reader(stderr_reader, "stderr")?;
    let output_capture = OutputCaptureSummary {
        stdout: stdout_capture.terminal,
        stderr: stderr_capture.terminal,
    };
    on_event(&ScriptLifecycleEvent::OutputCaptureCompleted {
        schema_version: LIFECYCLE_EVENT_SCHEMA,
        summary: output_capture.clone(),
    })?;

    let result: SupervisorResult = read_json_no_follow_at(runtime_fd, RESULT_FILE_NAME)?;
    validate_supervisor_result(&result, &invocation.token)?;
    if let Some(error) = result.internal_error {
        return Err(ScriptSupervisorError::Internal(error));
    }
    let descriptor = result.descriptor.ok_or_else(|| {
        ScriptSupervisorError::Protocol("item supervisor result omitted the containment descriptor".to_string())
    })?;
    validate_descriptor(&descriptor, &invocation.token)?;
    if let Some(observed) = observed_descriptor {
        if observed != descriptor {
            return Err(ScriptSupervisorError::Protocol("item supervisor result changed the prepared containment identity".to_string()));
        }
    }
    let raw_wait_status = result.raw_wait_status.ok_or_else(|| {
        ScriptSupervisorError::Protocol("item supervisor result omitted the script wait status".to_string())
    })?;
    Ok(SupervisedOutcome {
        status: ExitStatus::from_raw(raw_wait_status),
        stdout_tail: stdout_capture.bytes,
        stderr_tail: stderr_capture.bytes,
        timed_out: result.timed_out,
        cancelled: result.cancelled,
        script_released: result.script_released,
        descriptor,
        containment_empty: result.containment_empty,
        background_descendants: result.background_descendants,
        output_capture,
    })
}


/// Recover a script containment recorded durably by the action journal.
///
/// Numeric process identifiers are never used without host/boot and process
/// start-identity validation. A surviving Linux cgroup is the only backend
/// that can be forcibly recovered after the trusted supervisor itself is gone.
pub fn recover_supervised(
    request: &ScriptRecoveryRequest,
) -> Result<ScriptRecoveryOutcome, ScriptSupervisorError> {
    recover_supervised_with_observer(request, |_| Ok(()))
}

/// Recover containment while durably reporting every recovery transition
/// before the corresponding signal or terminal classification is acted on.
pub fn recover_supervised_with_observer<E>(
    request: &ScriptRecoveryRequest,
    mut on_event: E,
) -> Result<ScriptRecoveryOutcome, ScriptSupervisorError>
where
    E: FnMut(&ScriptLifecycleEvent) -> Result<(), ScriptSupervisorError>,
{
    validate_descriptor(&request.descriptor, &request.token)?;
    if request.descriptor.token != request.token {
        return Err(ScriptSupervisorError::Protocol(
            "script recovery token does not match the containment descriptor".to_string(),
        ));
    }
    let local = current_host_boot_identity();
    if request.descriptor.host != local {
        return Ok(ScriptRecoveryOutcome::ManualRecoveryRequired(format!(
            "recorded script containment belongs to host/boot {}/{}/{} rather than the current {}/{}/{}; local signalling is forbidden",
            request.descriptor.host.machine_identity,
            request.descriptor.host.host_identity,
            request.descriptor.host.boot_identity,
            local.machine_identity,
            local.host_identity,
            local.boot_identity,
        )));
    }

    let runtime_directory = match open_private_runtime_directory(
        &request.runtime_directory,
        request.descriptor.runtime_directory,
    ) {
        Ok(directory) => directory,
        Err(ScriptSupervisorError::Io(error))
            if error.kind() == io::ErrorKind::NotFound =>
        {
            return Ok(ScriptRecoveryOutcome::ManualRecoveryRequired(
                "the recorded script runtime directory vanished before terminal state was durable"
                    .to_string(),
            ));
        }
        Err(error) => return Err(error),
    };
    let runtime_fd = runtime_directory.as_raw_fd();
    if let Some(result) = read_supervisor_result_if_present(runtime_fd, request)? {
        replay_recovery_result_events(&result, &request.descriptor, &mut on_event)?;
        if result.containment_empty {
            return Ok(if result.script_released {
                ScriptRecoveryOutcome::ContainmentAlreadyEmpty
            } else {
                ScriptRecoveryOutcome::ExecutionNeverReleased
            });
        }
        // A recorded helper error is diagnostic, not proof that a surviving
        // Linux cgroup is empty. Continue to the backend-specific recovery
        // handle; weaker backends will conservatively return manual recovery.
    }

    #[cfg(target_os = "linux")]
    {
        match request.descriptor.backend {
            ContainmentBackend::LinuxCgroupV2 => {
                return linux::recover_cgroup(request, &mut on_event)
            },
            ContainmentBackend::LinuxSubreaper => {
                if linux::stable_process_matches(&request.descriptor.supervisor)? {
                    let deadline = Instant::now() + RECOVERY_WAIT_GRACE;
                    while Instant::now() < deadline {
                        if let Some(result) =
                            read_supervisor_result_if_present(runtime_fd, request)?
                        {
                            replay_recovery_result_events(
                                &result,
                                &request.descriptor,
                                &mut on_event,
                            )?;
                            if result.containment_empty {
                                return Ok(if result.script_released {
                                    ScriptRecoveryOutcome::ContainmentTerminated
                                } else {
                                    ScriptRecoveryOutcome::ExecutionNeverReleased
                                });
                            }
                            if let Some(error) = result.internal_error {
                                return Ok(ScriptRecoveryOutcome::ManualRecoveryRequired(format!(
                                    "live Linux supervisor reported unresolved containment: {error}"
                                )));
                            }
                        }
                        thread::sleep(CONTROL_POLL_INTERVAL);
                    }
                    return Ok(ScriptRecoveryOutcome::ManualRecoveryRequired(
                        "the verified Linux subreaper supervisor remains alive, but it did not produce a containment-empty result within the recovery deadline"
                            .to_string(),
                    ));
                }
                return Ok(ScriptRecoveryOutcome::ManualRecoveryRequired(
                    "the Linux subreaper supervisor is no longer identifiable and no durable containment-empty result exists; descendants that escaped through an external broker cannot be ruled out"
                        .to_string(),
                ));
            }
            _ => {
                return Ok(ScriptRecoveryOutcome::ManualRecoveryRequired(
                    "the journal names a non-Linux containment backend on Linux".to_string(),
                ));
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        if request.descriptor.backend != ContainmentBackend::MacosSupervisor {
            return Ok(ScriptRecoveryOutcome::ManualRecoveryRequired(
                "the journal names a non-macOS containment backend on macOS".to_string(),
            ));
        }
        if macos::stable_process_matches(&request.descriptor.supervisor)? {
            let deadline = Instant::now() + RECOVERY_WAIT_GRACE;
            while Instant::now() < deadline {
                if let Some(result) = read_supervisor_result_if_present(runtime_fd, request)? {
                    replay_recovery_result_events(
                        &result,
                        &request.descriptor,
                        &mut on_event,
                    )?;
                    if result.containment_empty {
                        return Ok(if result.script_released {
                            ScriptRecoveryOutcome::ContainmentTerminated
                        } else {
                            ScriptRecoveryOutcome::ExecutionNeverReleased
                        });
                    }
                    if let Some(error) = result.internal_error {
                        return Ok(ScriptRecoveryOutcome::ManualRecoveryRequired(format!(
                            "live macOS supervisor reported unresolved containment: {error}"
                        )));
                    }
                }
                thread::sleep(CONTROL_POLL_INTERVAL);
            }
            return Ok(ScriptRecoveryOutcome::ManualRecoveryRequired(
                "the verified macOS supervisor remains alive, but containment emptiness was not durably confirmed within the recovery deadline"
                    .to_string(),
            ));
        }
        return Ok(ScriptRecoveryOutcome::ManualRecoveryRequired(
            "the macOS supervisor is gone and no durable containment-empty result exists; macOS provides no cgroup-equivalent recovery handle, so automatic signalling is unsafe"
                .to_string(),
        ));
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Ok(ScriptRecoveryOutcome::ManualRecoveryRequired(
            "script containment recovery is unsupported on this platform".to_string(),
        ))
    }
}

fn replay_recovery_result_events(
    result: &SupervisorResult,
    descriptor: &ContainmentDescriptor,
    on_event: &mut dyn FnMut(&ScriptLifecycleEvent) -> Result<(), ScriptSupervisorError>,
) -> Result<(), ScriptSupervisorError> {
    if let Some(termination) = result.termination.as_ref() {
        on_event(&ScriptLifecycleEvent::TerminationRequested {
            schema_version: LIFECYCLE_EVENT_SCHEMA,
            reason: termination.reason,
            graceful_deadline_unix_millis: termination.graceful_deadline_unix_millis,
        })?;
        if termination.forced {
            on_event(&ScriptLifecycleEvent::ForcedTerminationRequested {
                schema_version: LIFECYCLE_EVENT_SCHEMA,
                reason: termination.reason,
            })?;
        }
    }
    if let Some(raw_wait_status) = result.raw_wait_status {
        on_event(&ScriptLifecycleEvent::LeaderExited {
            schema_version: LIFECYCLE_EVENT_SCHEMA,
            raw_wait_status,
        })?;
    }
    if result.containment_empty {
        on_event(&ScriptLifecycleEvent::ContainmentEmpty {
            schema_version: LIFECYCLE_EVENT_SCHEMA,
            confidence: descriptor.confidence,
        })?;
    }
    Ok(())
}

/// Remove backend-owned containment artifacts only after the action journal has
/// durably recorded its terminal result and containment-empty proof.
pub fn cleanup_supervised(
    request: &ScriptRecoveryRequest,
) -> Result<(), ScriptSupervisorError> {
    validate_descriptor(&request.descriptor, &request.token)?;
    let local = current_host_boot_identity();
    if request.descriptor.host != local {
        return Err(ScriptSupervisorError::Protocol(
            "refusing to clean containment artifacts owned by another host or boot"
                .to_string(),
        ));
    }
    #[cfg(target_os = "linux")]
    {
        if request.descriptor.backend == ContainmentBackend::LinuxCgroupV2 {
            return linux::cleanup_cgroup(request);
        }
    }
    Ok(())
}

fn read_supervisor_result_if_present(
    runtime_fd: RawFd,
    request: &ScriptRecoveryRequest,
) -> Result<Option<SupervisorResult>, ScriptSupervisorError> {
    if !entry_exists_no_follow_at(runtime_fd, RESULT_FILE_NAME)? {
        return Ok(None);
    }
    let result: SupervisorResult = read_json_no_follow_at(runtime_fd, RESULT_FILE_NAME)?;
    validate_supervisor_result(&result, &request.token)?;
    if let Some(descriptor) = result.descriptor.as_ref() {
        validate_descriptor(descriptor, &request.token)?;
        if descriptor != &request.descriptor {
            return Err(ScriptSupervisorError::Protocol(
                "script recovery result changed the durable containment descriptor".to_string(),
            ));
        }
    }
    Ok(Some(result))
}

/// Entry point used only by the hidden CLI subcommand.
pub fn run_internal_supervisor(
    runtime_fd: RawFd,
    control_fd: RawFd,
    event_fd: RawFd,
    script_fd: RawFd,
    working_directory_fd: RawFd,
    retained_lifetime_fds: &[RawFd],
) -> Result<(), ScriptSupervisorError> {
    if runtime_fd < 0 || script_fd < 0 || working_directory_fd < 0 {
        return Err(ScriptSupervisorError::Protocol(
            "invalid script runtime directory descriptor".to_string(),
        ));
    }
    // SAFETY: this hidden subcommand is the sole owner of its inherited runtime
    // directory descriptor. It remains open until the durable result is published.
    let runtime_directory = unsafe { File::from_raw_fd(runtime_fd) };
    // SAFETY: this hidden subcommand is the sole owner of the inherited
    // reviewed-script descriptor. It remains open until the launcher is
    // spawned and is never resolved again through the original pathname.
    let script_file = unsafe { File::from_raw_fd(script_fd) };
    if !script_file.metadata()?.is_file() {
        return Err(ScriptSupervisorError::Protocol(
            "inherited reviewed-script descriptor is not a regular file".to_string(),
        ));
    }
    // SAFETY: this hidden subcommand is the sole owner of the inherited exact
    // working-directory descriptor. It remains open until after `fchdir(2)`
    // establishes the helper's current directory.
    let working_directory = unsafe { File::from_raw_fd(working_directory_fd) };
    if !working_directory.metadata()?.is_dir() {
        return Err(ScriptSupervisorError::Protocol(
            "inherited working-directory descriptor is not a directory".to_string(),
        ));
    }
    // SAFETY: `working_directory` is a validated open directory descriptor.
    // Changing this dedicated helper's cwd is process-local and occurs before
    // any launcher or user code is spawned.
    if unsafe { libc::fchdir(working_directory.as_raw_fd()) } != 0 {
        return Err(ScriptSupervisorError::Io(io::Error::last_os_error()));
    }
    validate_private_runtime_directory_file(&runtime_directory, None)?;
    set_nonblocking(control_fd)?;
    // Lifecycle persistence is normally acknowledged immediately after a
    // journal fsync. Bound both directions so a live-but-wedged conversion
    // process cannot prevent the trusted supervisor from terminating an
    // already-running containment forever. Before user-code release, timeout
    // is a hard setup failure and the invocation remains exec-gated.
    set_socket_timeout(event_fd, libc::SO_RCVTIMEO, LIFECYCLE_IO_TIMEOUT)?;
    set_socket_timeout(event_fd, libc::SO_SNDTIMEO, LIFECYCLE_IO_TIMEOUT)?;
    // Parent/supervisor control, lifecycle, and runtime descriptors must never
    // leak into the launcher or target script.
    set_close_on_exec(runtime_directory.as_raw_fd())?;
    set_close_on_exec(control_fd)?;
    set_close_on_exec(event_fd)?;
    set_close_on_exec(script_file.as_raw_fd())?;
    set_close_on_exec(working_directory.as_raw_fd())?;
    // Reconstitute tonepoet-only lifetime holders.  Keeping these File values
    // alive makes a shared OFD lease survive the originating UI/session.
    // CLOEXEC is restored before any launcher is spawned, deliberately proving
    // that arbitrary external programs can close every inherited non-stdio FD
    // without releasing tonepoet ownership.
    let mut retained_lifetime_files = Vec::with_capacity(retained_lifetime_fds.len());
    for fd in retained_lifetime_fds {
        if *fd < 0 {
            return Err(ScriptSupervisorError::Protocol(
                "invalid retained lifetime descriptor".to_string(),
            ));
        }
        // SAFETY: this hidden helper exclusively owns each fd inherited for
        // lifetime retention from its tonepoet parent.
        let file = unsafe { File::from_raw_fd(*fd) };
        set_close_on_exec(file.as_raw_fd())?;
        retained_lifetime_files.push(file);
    }
    let mut spec: SupervisorSpec =
        read_json_no_follow_at(runtime_directory.as_raw_fd(), SPEC_FILE_NAME)?;
    if spec.schema_version != SUPERVISOR_RESULT_SCHEMA || !valid_token(&spec.token) {
        return Err(ScriptSupervisorError::Protocol(
            "invalid internal script-supervisor specification".to_string(),
        ));
    }
    validate_private_runtime_directory_file(&runtime_directory, Some(spec.runtime_identity))?;
    bind_post_album_environment_to_retained_cwd(&mut spec, &working_directory)?;
    let result = match run_helper(&spec, control_fd, event_fd, script_file.as_raw_fd()) {
        Ok(result) => result,
        Err(error) => SupervisorResult::internal(spec.token.clone(), error.to_string()),
    };
    write_private_json_atomic_at(
        runtime_directory.as_raw_fd(),
        RESULT_TEMP_FILE_NAME,
        RESULT_FILE_NAME,
        &result,
    )?;
    Ok(())
}


fn bind_post_album_environment_to_retained_cwd(
    spec: &mut SupervisorSpec,
    working_directory: &File,
) -> Result<(), ScriptSupervisorError> {
    if spec.environment.get("TONEPOET_PHASE").map(String::as_str) != Some("post") {
        return Ok(());
    }

    // A script's cwd is already installed from the retained directory
    // descriptor. The exported album path must identify that same object, not
    // the lexical pathname captured before publication. `current_dir()` asks
    // the kernel for the descriptor-backed cwd's current reachable pathname;
    // compare object identity before exporting it. If the directory has become
    // unreachable, fail closed rather than point user code at a replacement.
    let current = std::env::current_dir().map_err(|error| {
        ScriptSupervisorError::Protocol(format!(
            "post-action album directory has no verified current pathname: {error}"
        ))
    })?;
    let retained = working_directory.metadata()?;
    let resolved = fs::metadata(&current)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if retained.dev() != resolved.dev() || retained.ino() != resolved.ino() {
            return Err(ScriptSupervisorError::Protocol(format!(
                "post-action album pathname {} does not identify the retained working directory",
                current.display()
            )));
        }
    }
    #[cfg(not(unix))]
    {
        if !resolved.is_dir() || !retained.is_dir() {
            return Err(ScriptSupervisorError::Protocol(
                "post-action album pathname is not a directory".to_string(),
            ));
        }
    }
    let value = current.to_string_lossy().to_string();
    if value.contains('\0') {
        return Err(ScriptSupervisorError::Protocol(
            "verified post-action album pathname contains NUL".to_string(),
        ));
    }
    spec.environment
        .insert("TONEPOET_ALBUM_DIR".to_string(), value);
    Ok(())
}

/// Entry point used only by the hidden launcher subcommand.  The invocation is
/// received from the already-armed supervisor over an inherited private socket;
/// no pathname is reopened and no shell is involved.
pub fn run_internal_launcher(
    launch_fd: RawFd,
    cgroup_fd: Option<RawFd>,
    script_fd: RawFd,
) -> Result<(), ScriptSupervisorError> {
    if launch_fd < 0 || script_fd < 0 {
        return Err(ScriptSupervisorError::Protocol(
            "invalid script-launch channel descriptor".to_string(),
        ));
    }
    #[cfg(target_os = "linux")]
    if let Some(cgroup_fd) = cgroup_fd {
        linux::join_cgroup(cgroup_fd)?;
        // The directory capability is needed only to place this trusted
        // launcher into the already-created leaf.  Restore close-on-exec
        // before receiving the invocation so untrusted user code never
        // inherits a writable handle to its containment control directory.
        set_close_on_exec(cgroup_fd)?;
    }
    #[cfg(not(target_os = "linux"))]
    if cgroup_fd.is_some() {
        return Err(ScriptSupervisorError::Protocol(
            "cgroup setup is valid only on Linux".to_string(),
        ));
    }

    // SAFETY: this hidden subcommand is the sole owner of the inherited launch
    // descriptor and a duplicate of the retained reviewed-script descriptor.
    // The script descriptor deliberately remains open across fexecve so a
    // shebang interpreter can consume it without reopening the pathname.
    let mut channel = unsafe { File::from_raw_fd(launch_fd) };
    let script_file = unsafe { File::from_raw_fd(script_fd) };
    if !script_file.metadata()?.is_file() {
        return Err(ScriptSupervisorError::Protocol(
            "launcher reviewed-script descriptor is not a regular file".to_string(),
        ));
    }
    clear_close_on_exec(script_file.as_raw_fd())?;
    // The supervisor does not release any invocation bytes until this process
    // has completed all platform containment setup.  In particular, Linux
    // sends this only after cgroup.procs accepted the launcher's PID.
    channel.write_all(&[LAUNCHER_READY])?;
    let mut length_bytes = [0_u8; 4];
    channel.read_exact(&mut length_bytes)?;
    let length = u32::from_be_bytes(length_bytes) as usize;
    if length == 0 || length > MAX_LAUNCH_SPEC_BYTES {
        return Err(ScriptSupervisorError::Protocol(format!(
            "script-launch specification length {length} is outside the safety bound"
        )));
    }
    let mut bytes = vec![0_u8; length];
    channel.read_exact(&mut bytes)?;
    let spec: SupervisorSpec = serde_json::from_slice(&bytes)?;
    if spec.schema_version != SUPERVISOR_RESULT_SCHEMA || !valid_token(&spec.token) {
        return Err(ScriptSupervisorError::Protocol(
            "invalid script-launch specification".to_string(),
        ));
    }
    drop(channel);

    exec_retained_script(&spec, script_file.as_raw_fd())
}

fn run_helper(
    spec: &SupervisorSpec,
    control_fd: RawFd,
    event_fd: RawFd,
    script_fd: RawFd,
) -> Result<SupervisorResult, ScriptSupervisorError> {
    #[cfg(target_os = "linux")]
    {
        return linux::run(spec, control_fd, event_fd, script_fd);
    }
    #[cfg(target_os = "macos")]
    {
        if spec.containment_preference == ContainmentPreference::RequireLinuxCgroupV2 {
            return Err(ScriptSupervisorError::Internal(
                "Linux cgroup-v2 containment was required, but this host is macOS; refusing to release user code"
                    .to_string(),
            ));
        }
        return macos::run(spec, control_fd, event_fd, script_fd);
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (spec, control_fd, event_fd, script_fd);
        Err(ScriptSupervisorError::Internal(
            "conversion action scripts require Linux or macOS containment".to_string(),
        ))
    }
}

fn exec_retained_script(
    spec: &SupervisorSpec,
    script_fd: RawFd,
) -> Result<(), ScriptSupervisorError> {
    // The trusted supervisor already performed `fchdir(2)` through the exact
    // retained working-directory descriptor. The launcher inherits that cwd;
    // never resolve `spec.working_directory` again here.

    let mut argv_storage = Vec::with_capacity(spec.args.len() + 1);
    argv_storage.push(CString::new(spec.script.as_os_str().as_bytes()).map_err(|_| {
        ScriptSupervisorError::Protocol("script argv[0] contains NUL".to_string())
    })?);
    for argument in &spec.args {
        argv_storage.push(CString::new(argument.as_bytes()).map_err(|_| {
            ScriptSupervisorError::Protocol("script argument contains NUL".to_string())
        })?);
    }
    let mut argv = argv_storage
        .iter()
        .map(|value| value.as_ptr())
        .collect::<Vec<_>>();
    argv.push(std::ptr::null());

    let mut env_storage = Vec::with_capacity(spec.environment.len());
    for (key, value) in &spec.environment {
        if key.is_empty() || key.contains('=') {
            return Err(ScriptSupervisorError::Protocol(
                "script environment contains an invalid key".to_string(),
            ));
        }
        env_storage.push(CString::new(format!("{key}={value}")).map_err(|_| {
            ScriptSupervisorError::Protocol("script environment contains NUL".to_string())
        })?);
    }
    let mut envp = env_storage
        .iter()
        .map(|value| value.as_ptr())
        .collect::<Vec<_>>();
    envp.push(std::ptr::null());

    // SAFETY: argv/envp are NUL-terminated arrays backed by live CStrings;
    // script_fd names the retained reviewed regular file and has CLOEXEC
    // cleared so shebang interpreters can consume it.
    #[cfg(target_os = "linux")]
    unsafe {
        libc::fexecve(script_fd, argv.as_ptr(), envp.as_ptr());
    }
    #[cfg(target_os = "macos")]
    {
        // macOS does not provide a portable fexecve binding. /dev/fd resolves
        // the retained open file description rather than the original ambient
        // pathname, preserving exact reviewed-object execution semantics.
        let descriptor_path = CString::new(format!("/dev/fd/{script_fd}"))
            .map_err(|_| ScriptSupervisorError::Protocol(
                "retained script descriptor path contains NUL".to_string(),
            ))?;
        unsafe {
            libc::execve(descriptor_path.as_ptr(), argv.as_ptr(), envp.as_ptr());
        }
    }
    Err(io::Error::last_os_error().into())
}

fn duration_millis_u64(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn unix_millis_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .unwrap_or(u64::MAX)
}

fn graceful_deadline_unix_millis() -> u64 {
    unix_millis_now().saturating_add(duration_millis_u64(TERM_GRACE))
}

fn control_reason(control: u8) -> Option<TerminationReason> {
    match control {
        CONTROL_CANCEL => Some(TerminationReason::Cancellation),
        CONTROL_TIMEOUT => Some(TerminationReason::Timeout),
        CONTROL_PARENT_GONE => Some(TerminationReason::ParentDisconnected),
        _ => None,
    }
}

fn valid_token(token: &str) -> bool {
    token.len() == 32 && token.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn clear_close_on_exec(fd: RawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn set_close_on_exec(fd: RawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn set_socket_timeout(fd: RawFd, option: libc::c_int, duration: Duration) -> io::Result<()> {
    let timeout = libc::timeval {
        tv_sec: duration
            .as_secs()
            .min(libc::time_t::MAX as u64) as libc::time_t,
        tv_usec: duration.subsec_micros() as libc::suseconds_t,
    };
    let result = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            option,
            (&timeout as *const libc::timeval).cast(),
            std::mem::size_of::<libc::timeval>() as libc::socklen_t,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn poll_control(fd: RawFd) -> Result<Option<u8>, ScriptSupervisorError> {
    let mut bytes = [0_u8; 16];
    let count = unsafe { libc::read(fd, bytes.as_mut_ptr().cast(), bytes.len()) };
    if count > 0 {
        for byte in &bytes[..count as usize] {
            if matches!(*byte, CONTROL_CANCEL | CONTROL_TIMEOUT) {
                return Ok(Some(*byte));
            }
        }
        return Ok(None);
    }
    if count == 0 {
        // The conversion process disappeared or abandoned the invocation.
        // Treat control-channel EOF as cancellation so this helper does not
        // become an orphaned script supervisor.
        return Ok(Some(CONTROL_PARENT_GONE));
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::EAGAIN) => Ok(None),
        Some(libc::EINTR) => Ok(None),
        _ => Err(error.into()),
    }
}

fn send_control(stream: &mut UnixStream, byte: u8) -> Result<(), ScriptSupervisorError> {
    match stream.write_all(&[byte]) {
        Ok(()) => Ok(()),
        Err(error) if matches!(error.kind(), io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset) => {
            // The helper may have completed between try_wait and this write.
            // Its authenticated result record remains authoritative.
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

#[derive(Debug)]
struct TailCapture {
    bytes: Vec<u8>,
    terminal: OutputCaptureTerminal,
}

fn spawn_tail_reader<R>(
    mut reader: R,
    stop: Arc<AtomicBool>,
) -> Result<thread::JoinHandle<io::Result<TailCapture>>, ScriptSupervisorError>
where
    R: Read + AsRawFd + Send + 'static,
{
    set_nonblocking(reader.as_raw_fd())?;
    Ok(thread::spawn(move || {
        let mut tail = VecDeque::with_capacity(STDIO_TAIL_LIMIT);
        let mut buffer = [0_u8; 8192];
        let mut stopping_since = None;
        let mut truncated = false;
        let mut abandoned = false;
        loop {
            if stop.load(Ordering::Acquire) && stopping_since.is_none() {
                stopping_since = Some(Instant::now());
            }
            if stopping_since
                .map(|started| started.elapsed() >= TAIL_DRAIN_GRACE)
                .unwrap_or(false)
            {
                abandoned = true;
                break;
            }
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => {
                    for byte in &buffer[..count] {
                        if tail.len() == STDIO_TAIL_LIMIT {
                            tail.pop_front();
                            truncated = true;
                        }
                        tail.push_back(*byte);
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(CONTROL_POLL_INTERVAL);
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
        let terminal = if abandoned {
            OutputCaptureTerminal::Abandoned
        } else if truncated {
            OutputCaptureTerminal::Truncated
        } else {
            OutputCaptureTerminal::Complete
        };
        Ok(TailCapture {
            bytes: tail.into_iter().collect(),
            terminal,
        })
    }))
}

fn join_tail_reader(
    reader: thread::JoinHandle<io::Result<TailCapture>>,
    stream: &str,
) -> Result<TailCapture, ScriptSupervisorError> {
    reader
        .join()
        .map_err(|_| ScriptSupervisorError::Internal(format!("{stream} reader panicked")))?
        .map_err(ScriptSupervisorError::Io)
}

fn join_optional_tail_reader(
    reader: Option<thread::JoinHandle<io::Result<TailCapture>>>,
    stream: &str,
) -> Result<TailCapture, ScriptSupervisorError> {
    match reader {
        Some(reader) => join_tail_reader(reader, stream),
        None => Ok(TailCapture { bytes: Vec::new(), terminal: OutputCaptureTerminal::Complete }),
    }
}

#[derive(Default)]
struct EventFrameReader {
    buffer: Vec<u8>,
}

impl EventFrameReader {
    fn read_available(
        &mut self,
        stream: &mut UnixStream,
    ) -> Result<Vec<ScriptLifecycleEvent>, ScriptSupervisorError> {
        let mut chunk = [0_u8; 8192];
        loop {
            match stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(count) => self.buffer.extend_from_slice(&chunk[..count]),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error.into()),
            }
        }
        let mut events = Vec::new();
        loop {
            if self.buffer.len() < 4 {
                break;
            }
            let length = u32::from_be_bytes(self.buffer[..4].try_into().unwrap()) as usize;
            if length == 0 || length > MAX_EVENT_BYTES {
                return Err(ScriptSupervisorError::Protocol(format!(
                    "script lifecycle event length {length} is outside the safety bound"
                )));
            }
            if self.buffer.len() < 4 + length {
                break;
            }
            let event: ScriptLifecycleEvent = serde_json::from_slice(&self.buffer[4..4 + length])?;
            self.buffer.drain(..4 + length);
            events.push(event);
        }
        Ok(events)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LifecycleAcknowledgement {
    Acknowledged,
    Aborted,
    Disconnected,
}

fn emit_lifecycle_event(
    event_fd: RawFd,
    event: &ScriptLifecycleEvent,
) -> Result<LifecycleAcknowledgement, ScriptSupervisorError> {
    if event.schema_version() != LIFECYCLE_EVENT_SCHEMA {
        return Err(ScriptSupervisorError::Protocol(
            "attempted to emit a lifecycle event with the wrong schema".to_string(),
        ));
    }
    let bytes = serde_json::to_vec(event)?;
    if bytes.is_empty() || bytes.len() > MAX_EVENT_BYTES {
        return Err(ScriptSupervisorError::Protocol(format!(
            "script lifecycle event is {} bytes; maximum is {MAX_EVENT_BYTES}",
            bytes.len()
        )));
    }
    let length = u32::try_from(bytes.len()).map_err(|_| {
        ScriptSupervisorError::Protocol("script lifecycle event is too large".to_string())
    })?;
    if let Err(error) = write_all_fd(event_fd, &length.to_be_bytes()) {
        if lifecycle_channel_disconnected(&error) {
            return Ok(LifecycleAcknowledgement::Disconnected);
        }
        return Err(error.into());
    }
    if let Err(error) = write_all_fd(event_fd, &bytes) {
        if lifecycle_channel_disconnected(&error) {
            return Ok(LifecycleAcknowledgement::Disconnected);
        }
        return Err(error.into());
    }
    let mut ack = [0_u8; 1];
    loop {
        let count = unsafe { libc::read(event_fd, ack.as_mut_ptr().cast(), 1) };
        if count == 1 {
            return match ack[0] {
                EVENT_ACK => Ok(LifecycleAcknowledgement::Acknowledged),
                EVENT_ABORT => Ok(LifecycleAcknowledgement::Aborted),
                // A foreign acknowledgement can never authorize exec.
                // Treat it as an explicit abort so the backend terminates the
                // gated launcher rather than unwinding with a live child.
                _ => Ok(LifecycleAcknowledgement::Aborted),
            };
        }
        if count == 0 {
            return Ok(LifecycleAcknowledgement::Disconnected);
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            continue;
        }
        if lifecycle_channel_disconnected(&error) {
            return Ok(LifecycleAcknowledgement::Disconnected);
        }
        return Err(error.into());
    }
}

fn lifecycle_channel_disconnected(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::NotConnected
            | io::ErrorKind::TimedOut
            | io::ErrorKind::WouldBlock
    )
}

fn emit_lifecycle_best_effort(event_fd: RawFd, event: &ScriptLifecycleEvent) {
    // Once user code may have run, loss of the parent/event channel must never
    // suppress containment termination. A live parent still persists and ACKs
    // the event; a dead or failing parent causes recovery to rely on the
    // backend handle/result record instead.
    let _ = emit_lifecycle_event(event_fd, event);
}

fn write_all_fd(fd: RawFd, mut bytes: &[u8]) -> io::Result<()> {
    while !bytes.is_empty() {
        let count = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
        if count > 0 {
            bytes = &bytes[count as usize..];
            continue;
        }
        if count == 0 {
            return Err(io::Error::new(io::ErrorKind::WriteZero, "short lifecycle write"));
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            continue;
        }
        return Err(error);
    }
    Ok(())
}

fn static_cstring(name: &str) -> CString {
    CString::new(name).expect("static supervisor record name contains no NUL")
}

fn write_private_json_new_at<T: Serialize>(
    directory_fd: RawFd,
    name: &str,
    value: &T,
) -> Result<(), ScriptSupervisorError> {
    let bytes = serde_json::to_vec(value)?;
    let name = static_cstring(name);
    let fd = unsafe {
        libc::openat(
            directory_fd,
            name.as_ptr(),
            libc::O_WRONLY
                | libc::O_CREAT
                | libc::O_EXCL
                | libc::O_NOFOLLOW
                | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error().into());
    }
    let mut file = unsafe { File::from_raw_fd(fd) };
    file.write_all(&bytes)?;
    file.sync_all()?;
    Ok(())
}

fn write_private_json_atomic_at<T: Serialize>(
    directory_fd: RawFd,
    temporary_name: &str,
    destination_name: &str,
    value: &T,
) -> Result<(), ScriptSupervisorError> {
    write_private_json_new_at(directory_fd, temporary_name, value)?;
    let temporary = static_cstring(temporary_name);
    let destination = static_cstring(destination_name);
    // A hard-link publication is a portable same-directory no-clobber commit
    // for a regular file. It cannot overwrite a foreign result record.
    let linked = unsafe {
        libc::linkat(
            directory_fd,
            temporary.as_ptr(),
            directory_fd,
            destination.as_ptr(),
            0,
        )
    };
    if linked != 0 {
        let error = io::Error::last_os_error();
        let _ = unsafe { libc::unlinkat(directory_fd, temporary.as_ptr(), 0) };
        return Err(error.into());
    }
    sync_directory(directory_fd)?;
    let unlinked = unsafe { libc::unlinkat(directory_fd, temporary.as_ptr(), 0) };
    if unlinked != 0 {
        return Err(io::Error::last_os_error().into());
    }
    sync_directory(directory_fd)?;
    Ok(())
}

fn read_json_no_follow_at<T: for<'de> Deserialize<'de>>(
    directory_fd: RawFd,
    name: &str,
) -> Result<T, ScriptSupervisorError> {
    let name_c = static_cstring(name);
    let fd = unsafe {
        libc::openat(
            directory_fd,
            name_c.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error().into());
    }
    let mut file = unsafe { File::from_raw_fd(fd) };
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > MAX_LAUNCH_SPEC_BYTES as u64 {
        return Err(ScriptSupervisorError::Protocol(format!(
            "internal supervisor record {name} is not a bounded regular file"
        )));
    }
    if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 {
        return Err(ScriptSupervisorError::Protocol(format!(
            "internal supervisor record {name} has unsafe ownership or permissions"
        )));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.read_to_end(&mut bytes)?;
    Ok(serde_json::from_slice(&bytes)?)
}

fn entry_exists_no_follow_at(directory_fd: RawFd, name: &str) -> io::Result<bool> {
    let name = static_cstring(name);
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    let result = unsafe {
        libc::fstatat(
            directory_fd,
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result == 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ENOENT) {
        Ok(false)
    } else {
        Err(error)
    }
}

fn sync_directory(directory_fd: RawFd) -> io::Result<()> {
    let result = unsafe { libc::fsync(directory_fd) };
    if result == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    #[cfg(target_os = "macos")]
    if matches!(error.raw_os_error(), Some(libc::EINVAL) | Some(libc::ENOTSUP)) {
        return Ok(());
    }
    Err(error)
}

fn open_private_runtime_directory(
    path: &Path,
    expected: RuntimeDirectoryIdentity,
) -> Result<File, ScriptSupervisorError> {
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    validate_private_runtime_directory_file(&directory, Some(expected))?;
    Ok(directory)
}

fn validate_private_runtime_directory_file(
    directory: &File,
    expected: Option<RuntimeDirectoryIdentity>,
) -> Result<RuntimeDirectoryIdentity, ScriptSupervisorError> {
    let metadata = directory.metadata()?;
    if !metadata.file_type().is_dir() {
        return Err(ScriptSupervisorError::Protocol(
            "script runtime descriptor is not a directory".to_string(),
        ));
    }
    if metadata.uid() != unsafe { libc::geteuid() } {
        return Err(ScriptSupervisorError::Protocol(
            "script runtime directory is not owned by the current user".to_string(),
        ));
    }
    if metadata.mode() & 0o077 != 0 {
        return Err(ScriptSupervisorError::Protocol(
            "script runtime directory permissions are broader than 0700".to_string(),
        ));
    }
    let identity = RuntimeDirectoryIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    };
    if let Some(expected) = expected {
        if identity != expected {
            return Err(ScriptSupervisorError::Protocol(
                "script runtime pathname/descriptor no longer identifies the planned directory"
                    .to_string(),
            ));
        }
    }
    Ok(identity)
}

fn read_trimmed(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub(crate) fn current_host_boot_identity() -> HostBootIdentity {
    static IDENTITY: OnceLock<HostBootIdentity> = OnceLock::new();
    IDENTITY
        .get_or_init(|| {
            #[cfg(target_os = "macos")]
            let machine_identity = macos::machine_identity()
                .or_else(|| read_trimmed(Path::new("/etc/machine-id")))
                .unwrap_or_else(|| "machine-id-unavailable".to_string());
            #[cfg(not(target_os = "macos"))]
            let machine_identity = read_trimmed(Path::new("/etc/machine-id"))
                .unwrap_or_else(|| "machine-id-unavailable".to_string());
            let host_identity = read_trimmed(Path::new("/etc/hostname"))
                .or_else(|| std::env::var("HOSTNAME").ok().filter(|value| !value.is_empty()))
                .unwrap_or_else(|| "host-unavailable".to_string());
            #[cfg(target_os = "linux")]
            let boot_identity = read_trimmed(Path::new("/proc/sys/kernel/random/boot_id"))
                .unwrap_or_else(|| "boot-id-unavailable".to_string());
            #[cfg(target_os = "macos")]
            let boot_identity = macos::boot_identity()
                .unwrap_or_else(|| "boot-id-unavailable".to_string());
            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
            let boot_identity = "boot-id-unavailable".to_string();
            HostBootIdentity {
                machine_identity,
                host_identity,
                boot_identity,
            }
        })
        .clone()
}

/// Return a stable start identity for an arbitrary local process. `Ok(None)`
/// means the process is proven absent; an error means liveness is indeterminate
/// and callers must fail closed. Linux uses `/proc/<pid>/stat`; macOS uses
/// `proc_pidinfo(PROC_PIDTBSDINFO)` and the kernel-reported start timeval.
pub(crate) fn local_process_start_identity(pid: u32) -> io::Result<Option<String>> {
    #[cfg(target_os = "linux")]
    {
        let path = format!("/proc/{pid}/stat");
        let stat = match fs::read_to_string(&path) {
            Ok(stat) => stat,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let close = stat.rfind(") ").ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "malformed /proc process stat")
        })?;
        let fields: Vec<&str> = stat[close + 2..].split_whitespace().collect();
        let start = fields.get(19).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "missing process starttime in /proc stat")
        })?;
        return Ok(Some((*start).to_string()));
    }

    #[cfg(target_os = "macos")]
    {
        let pid = i32::try_from(pid).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "macOS PID is out of range")
        })?;
        return macos::process_identity(pid).map(|identity| {
            identity.map(|identity| format!("{}:{}", identity.start_sec, identity.start_usec))
        });
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        if pid == std::process::id() {
            Ok(Some(format!("self-{pid}")))
        } else {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "arbitrary process start identity is unavailable on this platform",
            ))
        }
    }
}

fn validate_descriptor(
    descriptor: &ContainmentDescriptor,
    token: &str,
) -> Result<(), ScriptSupervisorError> {
    if descriptor.schema_version != LIFECYCLE_EVENT_SCHEMA
        || descriptor.token != token
        || !valid_token(&descriptor.token)
        || descriptor.supervisor.pid <= 1
        || descriptor.leader.pid <= 1
        || descriptor.supervisor.start_identity.is_empty()
        || descriptor.leader.start_identity.is_empty()
        || descriptor.runtime_directory.device == 0
        || descriptor.runtime_directory.inode == 0
        || descriptor.host.machine_identity.is_empty()
        || descriptor.host.machine_identity == "machine-id-unavailable"
        || descriptor.host.host_identity.is_empty()
        || descriptor.host.host_identity == "host-unavailable"
        || descriptor.host.boot_identity.is_empty()
        || descriptor.host.boot_identity == "boot-id-unavailable"
    {
        return Err(ScriptSupervisorError::Protocol(
            "invalid script containment descriptor".to_string(),
        ));
    }
    if let Some(cgroup) = descriptor.cgroup.as_ref() {
        let expected_name = format!("tonepoet-script-{token}");
        if cgroup.device == 0
            || cgroup.inode == 0
            || !cgroup.absolute_path.is_absolute()
            || cgroup.absolute_path.file_name().and_then(|name| name.to_str())
                != Some(expected_name.as_str())
        {
            return Err(ScriptSupervisorError::Protocol(
                "invalid Linux cgroup containment identity".to_string(),
            ));
        }
    }
    match descriptor.backend {
        ContainmentBackend::LinuxCgroupV2 if descriptor.cgroup.is_none() => Err(
            ScriptSupervisorError::Protocol(
                "Linux cgroup containment descriptor omitted its cgroup identity".to_string(),
            ),
        ),
        ContainmentBackend::LinuxSubreaper | ContainmentBackend::MacosSupervisor
            if descriptor.cgroup.is_some() =>
        {
            Err(ScriptSupervisorError::Protocol(
                "non-cgroup containment descriptor unexpectedly contains a cgroup identity"
                    .to_string(),
            ))
        }
        _ => Ok(()),
    }
}

fn spawn_launcher(
    cgroup_fd: Option<RawFd>,
    script_fd: RawFd,
    extra: impl Fn() -> io::Result<()> + Send + Sync + 'static,
) -> Result<(Child, UnixStream), ScriptSupervisorError> {
    let (release, launch_child) = UnixStream::pair()?;
    let launch_fd = launch_child.as_raw_fd();
    let current_executable = crate::reexec::current_executable_for_reexec().map_err(|error| {
        ScriptSupervisorError::Internal(format!(
            "cannot locate the current executable for the script launcher: {error}"
        ))
    })?;
    let mut command = Command::new(current_executable);
    command
        .arg(INTERNAL_LAUNCHER_SUBCOMMAND)
        .arg("--launch-fd")
        .arg(launch_fd.to_string())
        .arg("--script-fd")
        .arg(script_fd.to_string());
    if let Some(fd) = cgroup_fd {
        command.arg("--cgroup-fd").arg(fd.to_string());
    }
    command
        .env_clear()
        // The parent supervisor has already installed the invocation's exact
        // stdin policy: ordinary commands receive /dev/null, while pipeline
        // consumers receive their retained pipe endpoint. Inherit that
        // sanitized descriptor instead of unconditionally replacing it with
        // /dev/null at the containment boundary.
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    unsafe {
        command.pre_exec(move || {
            extra()?;
            if libc::getpgrp() != libc::getpid() && libc::setpgid(0, 0) != 0 {
                return Err(io::Error::last_os_error());
            }
            clear_close_on_exec(launch_fd)?;
            clear_close_on_exec(script_fd)?;
            if let Some(fd) = cgroup_fd {
                clear_close_on_exec(fd)?;
            }
            Ok(())
        });
    }
    let child = command.spawn()?;
    drop(launch_child);
    Ok((child, release))
}

fn wait_launcher_ready(channel: &mut UnixStream) -> Result<(), ScriptSupervisorError> {
    channel.set_read_timeout(Some(LAUNCHER_READY_TIMEOUT))?;
    let mut ready = [0_u8; 1];
    let result = channel.read_exact(&mut ready);
    channel.set_read_timeout(None)?;
    result?;
    if ready[0] != LAUNCHER_READY {
        return Err(ScriptSupervisorError::Protocol(
            "script launcher returned an invalid containment-ready acknowledgement".to_string(),
        ));
    }
    Ok(())
}

fn release_launcher(mut channel: UnixStream, spec: &SupervisorSpec) -> Result<(), ScriptSupervisorError> {
    let bytes = serde_json::to_vec(spec)?;
    if bytes.is_empty() || bytes.len() > MAX_LAUNCH_SPEC_BYTES {
        return Err(ScriptSupervisorError::Protocol(format!(
            "script-launch specification is {} bytes; maximum is {MAX_LAUNCH_SPEC_BYTES}",
            bytes.len()
        )));
    }
    let length = u32::try_from(bytes.len()).map_err(|_| {
        ScriptSupervisorError::Protocol("script-launch specification is too large".to_string())
    })?;
    channel.write_all(&length.to_be_bytes())?;
    channel.write_all(&bytes)?;
    Ok(())
}

fn raw_status(status: ExitStatus) -> i32 {
    status.into_raw()
}

fn signal_process_group(pid: u32, signal: i32) {
    if let Ok(pid) = i32::try_from(pid) {
        unsafe {
            libc::kill(-pid, signal);
        }
    }
}

fn emergency_kill_child_group(child: &mut Child) -> Vec<String> {
    let mut errors = Vec::new();
    signal_process_group(child.id(), libc::SIGKILL);
    if let Err(error) = child.kill() {
        if error.kind() != io::ErrorKind::InvalidInput {
            errors.push(format!("direct child kill failed: {error}"));
        }
    }
    if let Err(error) = child.wait() {
        errors.push(format!("direct child reap failed: {error}"));
    }
    errors
}

fn supervision_error_with_cleanup(
    primary: ScriptSupervisorError,
    cleanup_errors: Vec<String>,
) -> ScriptSupervisorError {
    if cleanup_errors.is_empty() {
        primary
    } else {
        ScriptSupervisorError::Internal(format!(
            "{primary}; emergency containment cleanup also reported: {}",
            cleanup_errors.join("; ")
        ))
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::ffi::CString;
    use std::os::unix::ffi::{OsStrExt, OsStringExt};

    const PR_SET_PDEATHSIG: libc::c_int = 1;
    const PR_SET_CHILD_SUBREAPER: libc::c_int = 36;

    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    pub(super) struct ProcessIdentity {
        pub(super) pid: i32,
        pub(super) start_ticks: u64,
    }

    #[derive(Debug, Clone, Copy)]
    pub(super) struct ProcessInfo {
        pub(super) identity: ProcessIdentity,
        pub(super) parent: i32,
        pub(super) group: i32,
        pub(super) state: u8,
    }

    struct CgroupLeaf {
        parent_path: PathBuf,
        parent: File,
        directory: File,
        name: OsString,
    }

    impl CgroupLeaf {
        fn create(token: &str) -> Result<Self, String> {
            let parent_path = current_cgroup_directory().map_err(|error| error.to_string())?;
            let parent = open_directory_no_follow(&parent_path).map_err(|error| {
                format!("cannot open delegated cgroup {}: {error}", parent_path.display())
            })?;
            let name = OsString::from(format!("tonepoet-script-{token}"));
            let c_name = cstring(&name).map_err(|error| error.to_string())?;
            let created = unsafe { libc::mkdirat(parent.as_raw_fd(), c_name.as_ptr(), 0o700) };
            if created != 0 {
                return Err(format!(
                    "cannot create a delegated cgroup beneath {}: {}",
                    parent_path.display(),
                    io::Error::last_os_error()
                ));
            }
            let directory = match openat_directory(parent.as_raw_fd(), &name) {
                Ok(directory) => directory,
                Err(error) => {
                    unsafe {
                        libc::unlinkat(parent.as_raw_fd(), c_name.as_ptr(), libc::AT_REMOVEDIR);
                    }
                    return Err(format!("cannot open the new delegated cgroup: {error}"));
                }
            };
            Ok(Self {
                parent_path,
                parent,
                directory,
                name,
            })
        }

        fn open_existing(
            identity: &LinuxCgroupIdentity,
            token: &str,
        ) -> Result<Self, ScriptSupervisorError> {
            let expected_name = OsString::from(format!("tonepoet-script-{token}"));
            if identity.absolute_path.file_name() != Some(expected_name.as_os_str()) {
                return Err(ScriptSupervisorError::Protocol(
                    "recorded cgroup path does not match the containment token".to_string(),
                ));
            }
            let parent_path = identity.absolute_path.parent().ok_or_else(|| {
                ScriptSupervisorError::Protocol(
                    "recorded cgroup path has no parent directory".to_string(),
                )
            })?.to_path_buf();
            let delegated_parent = current_cgroup_directory()?;
            if parent_path != delegated_parent {
                return Err(ScriptSupervisorError::Protocol(format!(
                    "recorded cgroup parent {} is not the current delegated cgroup {}; refusing cross-scope signalling",
                    parent_path.display(),
                    delegated_parent.display()
                )));
            }
            let parent = open_directory_no_follow(&parent_path)?;
            let directory = openat_directory(parent.as_raw_fd(), &expected_name)?;
            let metadata = directory.metadata()?;
            if metadata.dev() != identity.device || metadata.ino() != identity.inode {
                return Err(ScriptSupervisorError::Protocol(
                    "recorded cgroup pathname now identifies a different object".to_string(),
                ));
            }
            Ok(Self {
                parent_path,
                parent,
                directory,
                name: expected_name,
            })
        }

        fn fd(&self) -> RawFd {
            self.directory.as_raw_fd()
        }

        fn identity(&self) -> io::Result<LinuxCgroupIdentity> {
            let metadata = self.directory.metadata()?;
            Ok(LinuxCgroupIdentity {
                absolute_path: self.parent_path.join(&self.name),
                device: metadata.dev(),
                inode: metadata.ino(),
            })
        }

        fn populated(&self) -> io::Result<bool> {
            match read_control(self.fd(), "cgroup.events") {
                Ok(events) => {
                    for line in events.lines() {
                        let mut fields = line.split_whitespace();
                        if fields.next() == Some("populated") {
                            return Ok(fields.next() != Some("0"));
                        }
                    }
                    Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "cgroup.events omitted the populated field",
                    ))
                }
                Err(error) if error.raw_os_error() == Some(libc::ENOENT) => {
                    Ok(!self.member_pids()?.is_empty())
                }
                Err(error) => Err(error),
            }
        }

        fn member_pids(&self) -> io::Result<Vec<i32>> {
            let text = read_control(self.fd(), "cgroup.procs")?;
            let mut pids = Vec::new();
            for line in text.lines() {
                if let Ok(pid) = line.trim().parse::<i32>() {
                    if pid > 1 {
                        pids.push(pid);
                    }
                }
            }
            Ok(pids)
        }

        fn signal_members(&self, signal: i32) -> io::Result<()> {
            for pid in self.member_pids()? {
                if let Some(identity) = process_identity(pid)? {
                    signal_identity(identity, signal)?;
                }
            }
            Ok(())
        }

        fn kill_all(&self) -> io::Result<()> {
            match write_control(self.fd(), "cgroup.kill", b"1\n") {
                Ok(()) => Ok(()),
                Err(error)
                    if matches!(
                        error.raw_os_error(),
                        Some(libc::ENOENT)
                            | Some(libc::EOPNOTSUPP)
                            | Some(libc::EINVAL)
                            | Some(libc::EPERM)
                    ) =>
                {
                    self.signal_members(libc::SIGKILL)
                }
                Err(error) => Err(error),
            }
        }

        fn cleanup(self) -> io::Result<()> {
            if self.populated()? {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "delegated script cgroup is still populated",
                ));
            }
            let name = cstring(&self.name)?;
            let result = unsafe {
                libc::unlinkat(self.parent.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR)
            };
            if result == 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        }
    }

    pub(super) fn join_cgroup(cgroup_fd: RawFd) -> Result<(), ScriptSupervisorError> {
        if cgroup_fd < 0 {
            return Err(ScriptSupervisorError::Protocol(
                "invalid inherited cgroup descriptor".to_string(),
            ));
        }
        // The launcher entry point retains ownership of this inherited
        // descriptor. Borrow it here so the caller can restore FD_CLOEXEC
        // immediately after joining, preventing the target script from
        // inheriting cgroup control authority.
        let pid = format!("{}\n", std::process::id());
        write_control(cgroup_fd, "cgroup.procs", pid.as_bytes())?;
        // A child cgroup alone is not an inescapable boundary for a process
        // running as the same delegated user: it may be able to write an
        // ancestor cgroup.procs. Root a new cgroup namespace at the retained
        // leaf before acknowledging readiness. If the kernel/user namespace
        // policy disallows this, the supervisor kills this launcher and uses
        // the explicitly weaker subreaper backend instead.
        let namespace = unsafe { libc::unshare(libc::CLONE_NEWCGROUP) };
        if namespace != 0 {
            return Err(ScriptSupervisorError::Internal(format!(
                "cannot root a private cgroup namespace at the delegated leaf: {}",
                io::Error::last_os_error()
            )));
        }
        Ok(())
    }

    fn stable_identity(identity: ProcessIdentity) -> StableProcessIdentity {
        StableProcessIdentity {
            pid: u32::try_from(identity.pid).unwrap_or(0),
            start_identity: identity.start_ticks.to_string(),
        }
    }

    pub(super) fn supervisor_identity() -> Result<StableProcessIdentity, ScriptSupervisorError> {
        let pid = std::process::id() as i32;
        let identity = process_identity(pid)?.ok_or_else(|| {
            ScriptSupervisorError::Internal(
                "cannot obtain the Linux supervisor process identity".to_string(),
            )
        })?;
        Ok(stable_identity(identity))
    }

    fn make_descriptor(
        spec: &SupervisorSpec,
        backend: ContainmentBackend,
        confidence: ContainmentConfidence,
        leader: ProcessIdentity,
        cgroup: Option<LinuxCgroupIdentity>,
        warning: Option<String>,
    ) -> Result<ContainmentDescriptor, ScriptSupervisorError> {
        Ok(ContainmentDescriptor {
            schema_version: LIFECYCLE_EVENT_SCHEMA,
            token: spec.token.clone(),
            backend,
            confidence,
            host: current_host_boot_identity(),
            supervisor: supervisor_identity()?,
            leader: stable_identity(leader),
            runtime_directory: spec.runtime_identity,
            cgroup,
            session_id: Some(leader.pid),
            warning,
        })
    }

    fn emit_prepared(
        event_fd: RawFd,
        descriptor: &ContainmentDescriptor,
    ) -> Result<LifecycleAcknowledgement, ScriptSupervisorError> {
        emit_lifecycle_event(
            event_fd,
            &ScriptLifecycleEvent::ContainmentPrepared {
                schema_version: LIFECYCLE_EVENT_SCHEMA,
                descriptor: descriptor.clone(),
            },
        )
    }

    fn emit_released(
        event_fd: RawFd,
        leader: ProcessIdentity,
    ) -> Result<LifecycleAcknowledgement, ScriptSupervisorError> {
        emit_lifecycle_event(
            event_fd,
            &ScriptLifecycleEvent::UserCodeReleased {
                schema_version: LIFECYCLE_EVENT_SCHEMA,
                leader: stable_identity(leader),
            },
        )
    }

    fn observe_leader_exit(
        event_fd: RawFd,
        child: &mut Child,
        raw_wait_status: &mut Option<i32>,
    ) -> Result<(), ScriptSupervisorError> {
        if raw_wait_status.is_none() {
            if let Some(status) = child.try_wait()? {
                let raw = raw_status(status);
                *raw_wait_status = Some(raw);
                emit_lifecycle_best_effort(
                    event_fd,
                    &ScriptLifecycleEvent::LeaderExited {
                        schema_version: LIFECYCLE_EVENT_SCHEMA,
                        raw_wait_status: raw,
                    },
                );
            }
        }
        Ok(())
    }

    fn emit_containment_empty(
        event_fd: RawFd,
        confidence: ContainmentConfidence,
    ) -> Result<(), ScriptSupervisorError> {
        emit_lifecycle_best_effort(
            event_fd,
            &ScriptLifecycleEvent::ContainmentEmpty {
                schema_version: LIFECYCLE_EVENT_SCHEMA,
                confidence,
            },
        );
        Ok(())
    }

    pub(super) fn run(
        spec: &SupervisorSpec,
        control_fd: RawFd,
        event_fd: RawFd,
        script_fd: RawFd,
    ) -> Result<SupervisorResult, ScriptSupervisorError> {
        match spec.containment_preference {
            ContainmentPreference::ForceSupervisorFallback => run_subreaper(
                spec,
                control_fd,
                event_fd,
                script_fd,
                Some("supervisor fallback was explicitly selected".to_string()),
            ),
            ContainmentPreference::Auto | ContainmentPreference::RequireLinuxCgroupV2 => {
                match CgroupLeaf::create(&spec.token) {
                    Ok(leaf) => run_cgroup(
                        spec,
                        control_fd,
                        event_fd,
                        script_fd,
                        leaf,
                        spec.containment_preference == ContainmentPreference::Auto,
                    ),
                    Err(reason)
                        if spec.containment_preference == ContainmentPreference::Auto =>
                    {
                        run_subreaper(spec, control_fd, event_fd, script_fd, Some(reason))
                    }
                    Err(reason) => Err(ScriptSupervisorError::Internal(format!(
                        "required delegated cgroup-v2 containment is unavailable before script release: {reason}"
                    ))),
                }
            }
        }
    }

    fn run_cgroup(
        spec: &SupervisorSpec,
        control_fd: RawFd,
        event_fd: RawFd,
        script_fd: RawFd,
        leaf: CgroupLeaf,
        allow_fallback: bool,
    ) -> Result<SupervisorResult, ScriptSupervisorError> {
        let subreaper = unsafe { libc::prctl(PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) };
        if subreaper != 0 {
            let error = io::Error::last_os_error();
            let _ = leaf.cleanup();
            return Err(error.into());
        }
        let cgroup_fd = leaf.fd();
        let supervisor_pid = std::process::id() as i32;
        let (mut child, launch_channel) = match spawn_launcher(Some(cgroup_fd), script_fd, move || {
            set_parent_death_signal(supervisor_pid)
        }) {
            Ok(pair) => pair,
            Err(error) => {
                let _ = leaf.cleanup();
                return Err(error);
            }
        };
        let root = match process_identity(child.id() as i32) {
            Ok(Some(identity)) => identity,
            Ok(None) => {
                let cleanup = emergency_kill_child_group(&mut child);
                let _ = leaf.cleanup();
                return Err(supervision_error_with_cleanup(
                    ScriptSupervisorError::Internal(
                        "script launcher vanished before Linux cgroup tracking was armed"
                            .to_string(),
                    ),
                    cleanup,
                ));
            }
            Err(error) => {
                let mut cleanup = emergency_kill_child_group(&mut child);
                if let Err(cleanup_error) = leaf.cleanup() {
                    cleanup.push(format!("cgroup cleanup failed: {cleanup_error}"));
                }
                return Err(supervision_error_with_cleanup(error.into(), cleanup));
            }
        };
        let mut tracked = BTreeSet::from([root]);
        let mut launch_channel = launch_channel;
        if let Err(readiness_error) = wait_launcher_ready(&mut launch_channel) {
            let mut cleanup = emergency_kill_child_group(&mut child);
            if let Err(error) = wait_for_cgroup_empty(&leaf, KILL_GRACE, true) {
                cleanup.push(format!("cgroup did not empty after launcher setup failure: {error}"));
            }
            if let Err(error) = leaf.cleanup() {
                cleanup.push(format!("cgroup cleanup failed after launcher setup failure: {error}"));
            }
            if !cleanup.is_empty() {
                return Err(supervision_error_with_cleanup(readiness_error, cleanup));
            }
            if !allow_fallback {
                return Err(ScriptSupervisorError::Internal(format!(
                    "required delegated cgroup-v2 containment could not be armed before script release: {readiness_error}"
                )));
            }
            return run_subreaper(
                spec,
                control_fd,
                event_fd,
                script_fd,
                Some(format!(
                    "delegated cgroup launcher setup failed before script release: {readiness_error}"
                )),
            );
        }

        let descriptor = make_descriptor(
            spec,
            ContainmentBackend::LinuxCgroupV2,
            ContainmentConfidence::KernelEnforced,
            root,
            Some(leaf.identity()?),
            None,
        )?;
        let prepared_ack = emit_prepared(event_fd, &descriptor)?;
        if prepared_ack != LifecycleAcknowledgement::Acknowledged {
            let mut raw_wait_status = None;
            let termination = terminate_cgroup_and_tree(
                event_fd,
                TerminationReason::ParentDisconnected,
                &leaf,
                root.pid,
                supervisor_pid,
                &mut child,
                &mut tracked,
                &mut raw_wait_status,
            )?;
            let raw_wait_status = complete_leader_wait(event_fd, &mut child, raw_wait_status)?;
            // Once the descriptor has been emitted, retain the empty leaf
            // even when the parent explicitly aborts. Journal persistence may
            // have reached durable storage before a later persistence/finalize
            // error was reported to the callback; only the action recovery
            // path can decide whether that descriptor became authoritative.
            return Ok(SupervisorResult {
                schema_version: SUPERVISOR_RESULT_SCHEMA,
                token: spec.token.clone(),
                raw_wait_status: Some(raw_wait_status),
                timed_out: false,
                cancelled: true,
                script_released: false,
                termination: Some(termination),
                descriptor: Some(descriptor),
                containment_empty: true,
                background_descendants: false,
                internal_error: None,
            });
        }

        if let Some(control) = poll_control(control_fd)? {
            let reason = control_reason(control).unwrap_or(TerminationReason::ParentDisconnected);
            let mut raw_wait_status = None;
            let termination = terminate_cgroup_and_tree(
                event_fd,
                reason,
                &leaf,
                root.pid,
                supervisor_pid,
                &mut child,
                &mut tracked,
                &mut raw_wait_status,
            )?;
            let raw_wait_status = complete_leader_wait(event_fd, &mut child, raw_wait_status)?;
            return Ok(SupervisorResult {
                schema_version: SUPERVISOR_RESULT_SCHEMA,
                token: spec.token.clone(),
                raw_wait_status: Some(raw_wait_status),
                timed_out: reason == TerminationReason::Timeout,
                cancelled: matches!(
                    reason,
                    TerminationReason::Cancellation | TerminationReason::ParentDisconnected
                ),
                script_released: false,
                termination: Some(termination),
                descriptor: Some(descriptor),
                containment_empty: true,
                background_descendants: false,
                internal_error: None,
            });
        }

        release_launcher(launch_channel, spec)?;
        let _ = emit_released(event_fd, root);
        let started = Instant::now();
        let timeout = Duration::from_millis(spec.timeout_millis);
        let mut raw_wait_status = None;
        let mut timed_out = false;
        let mut cancelled = false;
        let mut background_descendants = false;
        let mut termination = None;
        let mut leader_exited_at: Option<Instant> = None;
        loop {
            observe_leader_exit(event_fd, &mut child, &mut raw_wait_status)?;
            refresh_linux_tree(
                root.pid,
                supervisor_pid,
                raw_wait_status.is_some(),
                &mut tracked,
            )?;
            let populated = leaf.populated()?;
            if raw_wait_status.is_some() && !populated && tracked.is_empty() {
                emit_containment_empty(event_fd, ContainmentConfidence::KernelEnforced)?;
                break;
            }
            if raw_wait_status.is_some() {
                let exited_at = leader_exited_at.get_or_insert_with(Instant::now);
                if exited_at.elapsed() >= BACKGROUND_EXIT_GRACE {
                    background_descendants = true;
                    termination = Some(terminate_cgroup_and_tree(
                        event_fd,
                        TerminationReason::LeaderExitedWithDescendants,
                        &leaf,
                        root.pid,
                        supervisor_pid,
                        &mut child,
                        &mut tracked,
                        &mut raw_wait_status,
                    )?);
                    break;
                }
            }
            if let Some(control) = poll_control(control_fd)? {
                let reason = control_reason(control).unwrap_or(TerminationReason::ParentDisconnected);
                timed_out = reason == TerminationReason::Timeout;
                cancelled = matches!(
                    reason,
                    TerminationReason::Cancellation | TerminationReason::ParentDisconnected
                );
                termination = Some(terminate_cgroup_and_tree(
                    event_fd,
                    reason,
                    &leaf,
                    root.pid,
                    supervisor_pid,
                    &mut child,
                    &mut tracked,
                    &mut raw_wait_status,
                )?);
                break;
            }
            if started.elapsed() >= timeout {
                timed_out = true;
                termination = Some(terminate_cgroup_and_tree(
                    event_fd,
                    TerminationReason::Timeout,
                    &leaf,
                    root.pid,
                    supervisor_pid,
                    &mut child,
                    &mut tracked,
                    &mut raw_wait_status,
                )?);
                break;
            }
            thread::sleep(CONTROL_POLL_INTERVAL);
        }
        let raw_wait_status = complete_leader_wait(event_fd, &mut child, raw_wait_status)?;
        reap_waitable_children();
        wait_for_cgroup_empty(&leaf, KILL_GRACE, false)?;
        Ok(SupervisorResult {
            schema_version: SUPERVISOR_RESULT_SCHEMA,
            token: spec.token.clone(),
            raw_wait_status: Some(raw_wait_status),
            timed_out,
            cancelled,
            script_released: true,
            termination,
            descriptor: Some(descriptor),
            containment_empty: true,
            background_descendants,
            internal_error: None,
        })
    }

    fn complete_leader_wait(
        event_fd: RawFd,
        child: &mut Child,
        raw_wait_status: Option<i32>,
    ) -> Result<i32, ScriptSupervisorError> {
        if let Some(raw) = raw_wait_status {
            return Ok(raw);
        }
        let raw = raw_status(child.wait()?);
        emit_lifecycle_best_effort(
            event_fd,
            &ScriptLifecycleEvent::LeaderExited {
                schema_version: LIFECYCLE_EVENT_SCHEMA,
                raw_wait_status: raw,
            },
        );
        Ok(raw)
    }

    fn terminate_cgroup_and_tree(
        event_fd: RawFd,
        reason: TerminationReason,
        leaf: &CgroupLeaf,
        root_pgid: i32,
        supervisor_pid: i32,
        child: &mut Child,
        tracked: &mut BTreeSet<ProcessIdentity>,
        raw_wait_status: &mut Option<i32>,
    ) -> Result<TerminationSummary, ScriptSupervisorError> {
        let graceful_deadline_unix_millis = graceful_deadline_unix_millis();
        emit_lifecycle_best_effort(
            event_fd,
            &ScriptLifecycleEvent::TerminationRequested {
                schema_version: LIFECYCLE_EVENT_SCHEMA,
                reason,
                graceful_deadline_unix_millis,
            },
        );
        signal_process_group(child.id(), libc::SIGTERM);
        leaf.signal_members(libc::SIGTERM)?;
        signal_identities(tracked, libc::SIGTERM)?;
        let term_deadline = Instant::now() + TERM_GRACE;
        while Instant::now() < term_deadline {
            observe_leader_exit(event_fd, child, raw_wait_status)?;
            refresh_linux_tree(root_pgid, supervisor_pid, raw_wait_status.is_some(), tracked)?;
            if !leaf.populated()? && tracked.is_empty() {
                reap_waitable_children_recording_leader(event_fd, child, raw_wait_status);
                emit_containment_empty(event_fd, ContainmentConfidence::KernelEnforced)?;
                return Ok(TerminationSummary {
                    reason,
                    graceful_deadline_unix_millis,
                    forced: false,
                });
            }
            leaf.signal_members(libc::SIGTERM)?;
            signal_identities(tracked, libc::SIGTERM)?;
            thread::sleep(CONTROL_POLL_INTERVAL);
        }

        emit_lifecycle_best_effort(
            event_fd,
            &ScriptLifecycleEvent::ForcedTerminationRequested {
                schema_version: LIFECYCLE_EVENT_SCHEMA,
                reason,
            },
        );
        leaf.kill_all()?;
        signal_process_group(child.id(), libc::SIGKILL);
        signal_identities(tracked, libc::SIGKILL)?;
        let kill_deadline = Instant::now() + KILL_GRACE;
        while Instant::now() < kill_deadline {
            observe_leader_exit(event_fd, child, raw_wait_status)?;
            refresh_linux_tree(root_pgid, supervisor_pid, raw_wait_status.is_some(), tracked)?;
            if !leaf.populated()? && tracked.is_empty() {
                reap_waitable_children_recording_leader(event_fd, child, raw_wait_status);
                emit_containment_empty(event_fd, ContainmentConfidence::KernelEnforced)?;
                return Ok(TerminationSummary {
                    reason,
                    graceful_deadline_unix_millis,
                    forced: true,
                });
            }
            let _ = leaf.kill_all();
            signal_identities(tracked, libc::SIGKILL)?;
            thread::sleep(CONTROL_POLL_INTERVAL);
        }
        Err(ScriptSupervisorError::Internal(
            "combined cgroup/subreaper process tree remained alive after SIGKILL".to_string(),
        ))
    }

    fn wait_for_cgroup_empty(
        leaf: &CgroupLeaf,
        grace: Duration,
        keep_killing: bool,
    ) -> io::Result<()> {
        let deadline = Instant::now() + grace;
        loop {
            if !leaf.populated()? {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "delegated script cgroup remained populated",
                ));
            }
            if keep_killing {
                let _ = leaf.kill_all();
            }
            thread::sleep(CONTROL_POLL_INTERVAL);
        }
    }

    fn run_subreaper(
        spec: &SupervisorSpec,
        control_fd: RawFd,
        event_fd: RawFd,
        script_fd: RawFd,
        fallback_reason: Option<String>,
    ) -> Result<SupervisorResult, ScriptSupervisorError> {
        let result = unsafe { libc::prctl(PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) };
        if result != 0 {
            return Err(io::Error::last_os_error().into());
        }
        let supervisor_pid = std::process::id() as i32;
        let (mut child, launch_channel) = spawn_launcher(None, script_fd, move || {
            set_parent_death_signal(supervisor_pid)
        })?;
        let root = match process_identity(child.id() as i32) {
            Ok(Some(identity)) => identity,
            Ok(None) => {
                let cleanup = emergency_kill_child_group(&mut child);
                return Err(supervision_error_with_cleanup(
                    ScriptSupervisorError::Internal(
                        "script launcher vanished before Linux subreaper tracking was armed"
                            .to_string(),
                    ),
                    cleanup,
                ));
            }
            Err(error) => {
                let cleanup = emergency_kill_child_group(&mut child);
                return Err(supervision_error_with_cleanup(error.into(), cleanup));
            }
        };
        let mut tracked = BTreeSet::from([root]);
        let mut launch_channel = launch_channel;
        wait_launcher_ready(&mut launch_channel)?;
        let warning = fallback_reason.map(|reason| {
            format!(
                "delegated cgroup v2 was unavailable ({reason}); used a dedicated child subreaper with PID-start-validated /proc tracking. This fallback cannot contain processes launched indirectly through an external service manager, and complete observation requires the user's own descendants to remain visible through /proc"
            )
        });
        let descriptor = make_descriptor(
            spec,
            ContainmentBackend::LinuxSubreaper,
            ContainmentConfidence::ProcessTreeObserved,
            root,
            None,
            warning,
        )?;
        if emit_prepared(event_fd, &descriptor)?
            != LifecycleAcknowledgement::Acknowledged
        {
            let mut raw_wait_status = None;
            let termination = terminate_linux_tree(
                event_fd,
                TerminationReason::ParentDisconnected,
                root.pid,
                supervisor_pid,
                &mut child,
                &mut tracked,
                &mut raw_wait_status,
            )?;
            let raw_wait_status = complete_leader_wait(event_fd, &mut child, raw_wait_status)?;
            return Ok(SupervisorResult {
                schema_version: SUPERVISOR_RESULT_SCHEMA,
                token: spec.token.clone(),
                raw_wait_status: Some(raw_wait_status),
                timed_out: false,
                cancelled: true,
                script_released: false,
                termination: Some(termination),
                descriptor: Some(descriptor),
                containment_empty: true,
                background_descendants: false,
                internal_error: None,
            });
        }
        if let Some(control) = poll_control(control_fd)? {
            let reason = control_reason(control).unwrap_or(TerminationReason::ParentDisconnected);
            let mut raw_wait_status = None;
            let termination = terminate_linux_tree(
                event_fd,
                reason,
                root.pid,
                supervisor_pid,
                &mut child,
                &mut tracked,
                &mut raw_wait_status,
            )?;
            let raw_wait_status = complete_leader_wait(event_fd, &mut child, raw_wait_status)?;
            return Ok(SupervisorResult {
                schema_version: SUPERVISOR_RESULT_SCHEMA,
                token: spec.token.clone(),
                raw_wait_status: Some(raw_wait_status),
                timed_out: reason == TerminationReason::Timeout,
                cancelled: matches!(
                    reason,
                    TerminationReason::Cancellation | TerminationReason::ParentDisconnected
                ),
                script_released: false,
                termination: Some(termination),
                descriptor: Some(descriptor),
                containment_empty: true,
                background_descendants: false,
                internal_error: None,
            });
        }

        release_launcher(launch_channel, spec)?;
        let _ = emit_released(event_fd, root);
        let started = Instant::now();
        let timeout = Duration::from_millis(spec.timeout_millis);
        let mut raw_wait_status = None;
        let mut timed_out = false;
        let mut cancelled = false;
        let mut background_descendants = false;
        let mut termination = None;
        let mut leader_exited_at: Option<Instant> = None;
        loop {
            observe_leader_exit(event_fd, &mut child, &mut raw_wait_status)?;
            refresh_linux_tree(
                root.pid,
                supervisor_pid,
                raw_wait_status.is_some(),
                &mut tracked,
            )?;
            if raw_wait_status.is_some() && tracked.is_empty() {
                emit_containment_empty(event_fd, ContainmentConfidence::ProcessTreeObserved)?;
                break;
            }
            if raw_wait_status.is_some() {
                let exited_at = leader_exited_at.get_or_insert_with(Instant::now);
                if exited_at.elapsed() >= BACKGROUND_EXIT_GRACE {
                    background_descendants = true;
                    termination = Some(terminate_linux_tree(
                        event_fd,
                        TerminationReason::LeaderExitedWithDescendants,
                        root.pid,
                        supervisor_pid,
                        &mut child,
                        &mut tracked,
                        &mut raw_wait_status,
                    )?);
                    break;
                }
            }
            if let Some(control) = poll_control(control_fd)? {
                let reason = control_reason(control).unwrap_or(TerminationReason::ParentDisconnected);
                timed_out = reason == TerminationReason::Timeout;
                cancelled = matches!(
                    reason,
                    TerminationReason::Cancellation | TerminationReason::ParentDisconnected
                );
                termination = Some(terminate_linux_tree(
                    event_fd,
                    reason,
                    root.pid,
                    supervisor_pid,
                    &mut child,
                    &mut tracked,
                    &mut raw_wait_status,
                )?);
                break;
            }
            if started.elapsed() >= timeout {
                timed_out = true;
                termination = Some(terminate_linux_tree(
                    event_fd,
                    TerminationReason::Timeout,
                    root.pid,
                    supervisor_pid,
                    &mut child,
                    &mut tracked,
                    &mut raw_wait_status,
                )?);
                break;
            }
            thread::sleep(CONTROL_POLL_INTERVAL);
        }
        let raw_wait_status = complete_leader_wait(event_fd, &mut child, raw_wait_status)?;
        reap_waitable_children();
        Ok(SupervisorResult {
            schema_version: SUPERVISOR_RESULT_SCHEMA,
            token: spec.token.clone(),
            raw_wait_status: Some(raw_wait_status),
            timed_out,
            cancelled,
            script_released: true,
            termination,
            descriptor: Some(descriptor),
            containment_empty: true,
            background_descendants,
            internal_error: None,
        })
    }

    fn terminate_linux_tree(
        event_fd: RawFd,
        reason: TerminationReason,
        root_pgid: i32,
        supervisor_pid: i32,
        child: &mut Child,
        tracked: &mut BTreeSet<ProcessIdentity>,
        raw_wait_status: &mut Option<i32>,
    ) -> Result<TerminationSummary, ScriptSupervisorError> {
        let graceful_deadline_unix_millis = graceful_deadline_unix_millis();
        emit_lifecycle_best_effort(
            event_fd,
            &ScriptLifecycleEvent::TerminationRequested {
                schema_version: LIFECYCLE_EVENT_SCHEMA,
                reason,
                graceful_deadline_unix_millis,
            },
        );
        signal_process_group(child.id(), libc::SIGTERM);
        signal_identities(tracked, libc::SIGTERM)?;
        let term_deadline = Instant::now() + TERM_GRACE;
        while Instant::now() < term_deadline {
            observe_leader_exit(event_fd, child, raw_wait_status)?;
            refresh_linux_tree(root_pgid, supervisor_pid, raw_wait_status.is_some(), tracked)?;
            if tracked.is_empty() {
                reap_waitable_children_recording_leader(event_fd, child, raw_wait_status);
                emit_containment_empty(event_fd, ContainmentConfidence::ProcessTreeObserved)?;
                return Ok(TerminationSummary {
                    reason,
                    graceful_deadline_unix_millis,
                    forced: false,
                });
            }
            signal_identities(tracked, libc::SIGTERM)?;
            thread::sleep(CONTROL_POLL_INTERVAL);
        }

        emit_lifecycle_best_effort(
            event_fd,
            &ScriptLifecycleEvent::ForcedTerminationRequested {
                schema_version: LIFECYCLE_EVENT_SCHEMA,
                reason,
            },
        );
        signal_process_group(child.id(), libc::SIGKILL);
        signal_identities(tracked, libc::SIGKILL)?;
        let kill_deadline = Instant::now() + KILL_GRACE;
        while Instant::now() < kill_deadline {
            observe_leader_exit(event_fd, child, raw_wait_status)?;
            refresh_linux_tree(root_pgid, supervisor_pid, raw_wait_status.is_some(), tracked)?;
            if tracked.is_empty() {
                reap_waitable_children_recording_leader(event_fd, child, raw_wait_status);
                emit_containment_empty(event_fd, ContainmentConfidence::ProcessTreeObserved)?;
                return Ok(TerminationSummary {
                    reason,
                    graceful_deadline_unix_millis,
                    forced: true,
                });
            }
            signal_identities(tracked, libc::SIGKILL)?;
            thread::sleep(CONTROL_POLL_INTERVAL);
        }
        Err(ScriptSupervisorError::Internal(
            "tracked Linux script process tree remained alive after SIGKILL".to_string(),
        ))
    }

    fn refresh_linux_tree(
        root_pgid: i32,
        supervisor_pid: i32,
        root_reaped: bool,
        tracked: &mut BTreeSet<ProcessIdentity>,
    ) -> io::Result<()> {
        let snapshot = process_snapshot()?;
        let by_pid: BTreeMap<i32, ProcessInfo> =
            snapshot.into_iter().map(|info| (info.identity.pid, info)).collect();
        tracked.retain(|identity| {
            by_pid
                .get(&identity.pid)
                .map(|info| info.identity == *identity && info.state != b'Z')
                .unwrap_or(false)
        });

        let mut changed = true;
        while changed {
            changed = false;
            for info in by_pid.values() {
                if info.identity.pid == supervisor_pid || info.state == b'Z' {
                    continue;
                }
                let parent_tracked = tracked.iter().any(|entry| entry.pid == info.parent);
                let reparented_to_supervisor = info.parent == supervisor_pid;
                let original_group = info.group == root_pgid;
                if (parent_tracked || reparented_to_supervisor || original_group)
                    && tracked.insert(info.identity)
                {
                    changed = true;
                }
            }
        }

        for info in by_pid.values() {
            if info.state == b'Z'
                && (root_reaped || info.identity.pid != root_pgid)
                && (tracked.contains(&info.identity)
                    || info.parent == supervisor_pid
                    || info.group == root_pgid)
            {
                reap_pid(info.identity.pid);
            }
        }
        tracked.retain(|identity| {
            process_identity(identity.pid)
                .ok()
                .flatten()
                .map(|current| current == *identity)
                .unwrap_or(false)
        });
        Ok(())
    }

    fn process_snapshot() -> io::Result<Vec<ProcessInfo>> {
        let mut result = Vec::new();
        for entry in fs::read_dir("/proc")? {
            let entry = entry?;
            let Some(text) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Ok(pid) = text.parse::<i32>() else {
                continue;
            };
            match process_info(pid) {
                Ok(Some(info)) => result.push(info),
                Ok(None) => {}
                Err(error)
                    if matches!(
                        error.raw_os_error(),
                        Some(libc::ENOENT) | Some(libc::ESRCH) | Some(libc::EACCES)
                    ) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(result)
    }

    pub(super) fn process_identity(pid: i32) -> io::Result<Option<ProcessIdentity>> {
        Ok(process_info(pid)?.map(|info| info.identity))
    }

    fn process_info(pid: i32) -> io::Result<Option<ProcessInfo>> {
        let stat = match fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(stat) => stat,
            Err(error)
                if matches!(
                    error.raw_os_error(),
                    Some(libc::ENOENT) | Some(libc::ESRCH)
                ) => return Ok(None),
            Err(error) => return Err(error),
        };
        Ok(Some(parse_proc_stat(pid, &stat)?))
    }

    pub(super) fn parse_proc_stat(pid: i32, stat: &str) -> io::Result<ProcessInfo> {
        let close = stat.rfind(')').ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "malformed /proc stat comm field")
        })?;
        let fields: Vec<&str> = stat[close + 1..].split_whitespace().collect();
        if fields.len() <= 19 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "truncated /proc stat record",
            ));
        }
        let state = fields[0].as_bytes().first().copied().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "missing /proc process state")
        })?;
        let parent = fields[1]
            .parse::<i32>()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid /proc parent PID"))?;
        let group = fields[2]
            .parse::<i32>()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid /proc process group"))?;
        let start_ticks = fields[19]
            .parse::<u64>()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid /proc start time"))?;
        Ok(ProcessInfo {
            identity: ProcessIdentity { pid, start_ticks },
            parent,
            group,
            state,
        })
    }

    fn signal_identities(
        identities: &BTreeSet<ProcessIdentity>,
        signal: i32,
    ) -> io::Result<()> {
        for identity in identities.iter().rev() {
            signal_identity(*identity, signal)?;
        }
        Ok(())
    }

    fn signal_identity(identity: ProcessIdentity, signal: i32) -> io::Result<()> {
        if process_identity(identity.pid)? != Some(identity) {
            return Ok(());
        }
        let pidfd = unsafe { libc::syscall(libc::SYS_pidfd_open, identity.pid, 0) } as i32;
        if pidfd >= 0 {
            let current = process_identity(identity.pid)?;
            if current != Some(identity) {
                unsafe { libc::close(pidfd) };
                return Ok(());
            }
            let result = unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    pidfd,
                    signal,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                )
            };
            let error = if result == 0 {
                None
            } else {
                Some(io::Error::last_os_error())
            };
            unsafe { libc::close(pidfd) };
            match error {
                None => return Ok(()),
                Some(error) if error.raw_os_error() == Some(libc::ESRCH) => return Ok(()),
                Some(error) => return Err(error),
            }
        }
        let open_error = io::Error::last_os_error();
        if !matches!(
            open_error.raw_os_error(),
            Some(libc::ENOSYS) | Some(libc::EINVAL) | Some(libc::EPERM)
        ) {
            if open_error.raw_os_error() == Some(libc::ESRCH) {
                return Ok(());
            }
            return Err(open_error);
        }
        if process_identity(identity.pid)? == Some(identity) {
            let result = unsafe { libc::kill(identity.pid, signal) };
            if result != 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    return Err(error);
                }
            }
        }
        Ok(())
    }

    fn reap_pid(pid: i32) {
        let mut status = 0;
        unsafe {
            libc::waitpid(pid, &mut status, libc::WNOHANG);
        }
    }

    fn reap_waitable_children() {
        loop {
            let mut status = 0;
            let result = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
            if result <= 0 {
                break;
            }
        }
    }

    /// Like `reap_waitable_children`, but never DISCARDS the leader: a
    /// global waitpid(-1) racing the leader's exit would otherwise steal the
    /// status, making the next `try_wait` fail with ECHILD and losing the
    /// exit code the protocol must record durably.
    fn reap_waitable_children_recording_leader(
        event_fd: RawFd,
        child: &Child,
        raw_wait_status: &mut Option<i32>,
    ) {
        let leader = child.id() as i32;
        loop {
            let mut status = 0;
            let result = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
            if result <= 0 {
                break;
            }
            if result == leader && raw_wait_status.is_none() {
                *raw_wait_status = Some(status);
                emit_lifecycle_best_effort(
                    event_fd,
                    &ScriptLifecycleEvent::LeaderExited {
                        schema_version: LIFECYCLE_EVENT_SCHEMA,
                        raw_wait_status: status,
                    },
                );
            }
        }
    }

    fn set_parent_death_signal(expected_parent: i32) -> io::Result<()> {
        let result = unsafe { libc::prctl(PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
        // PR_SET_PDEATHSIG is not retroactive. If the dedicated supervisor
        // died before this pre-exec hook armed the signal, refuse to exec the
        // launcher instead of leaving an unsupervised process behind.
        if unsafe { libc::getppid() } != expected_parent {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "script supervisor died before launcher parent-death protection was armed",
            ));
        }
        Ok(())
    }

    pub(super) fn stable_process_matches(
        expected: &StableProcessIdentity,
    ) -> Result<bool, ScriptSupervisorError> {
        let pid = i32::try_from(expected.pid).map_err(|_| {
            ScriptSupervisorError::Protocol("recorded Linux PID is out of range".to_string())
        })?;
        let expected_ticks = expected.start_identity.parse::<u64>().map_err(|_| {
            ScriptSupervisorError::Protocol(
                "recorded Linux process start identity is malformed".to_string(),
            )
        })?;
        Ok(process_identity(pid)?
            == Some(ProcessIdentity {
                pid,
                start_ticks: expected_ticks,
            }))
    }

    pub(super) fn recover_cgroup(
        request: &ScriptRecoveryRequest,
        on_event: &mut dyn FnMut(&ScriptLifecycleEvent) -> Result<(), ScriptSupervisorError>,
    ) -> Result<ScriptRecoveryOutcome, ScriptSupervisorError> {
        let identity = request.descriptor.cgroup.as_ref().ok_or_else(|| {
            ScriptSupervisorError::Protocol(
                "Linux cgroup containment descriptor omitted its cgroup identity".to_string(),
            )
        })?;
        let cgroup = match CgroupLeaf::open_existing(identity, &request.token) {
            Ok(cgroup) => cgroup,
            Err(ScriptSupervisorError::Io(error))
                if error.raw_os_error() == Some(libc::ENOENT) =>
            {
                return Ok(ScriptRecoveryOutcome::ManualRecoveryRequired(
                    "the recorded Linux cgroup vanished before a durable containment-empty result was written"
                        .to_string(),
                ));
            }
            Err(error) => return Err(error),
        };
        if !cgroup.populated()? {
            on_event(&ScriptLifecycleEvent::ContainmentEmpty {
                schema_version: LIFECYCLE_EVENT_SCHEMA,
                confidence: ContainmentConfidence::KernelEnforced,
            })?;
            return Ok(ScriptRecoveryOutcome::ContainmentAlreadyEmpty);
        }

        on_event(&ScriptLifecycleEvent::TerminationRequested {
            schema_version: LIFECYCLE_EVENT_SCHEMA,
            reason: TerminationReason::Recovery,
            graceful_deadline_unix_millis: graceful_deadline_unix_millis(),
        })?;
        cgroup.signal_members(libc::SIGTERM)?;
        let term_deadline = Instant::now() + TERM_GRACE;
        while Instant::now() < term_deadline {
            if !cgroup.populated()? {
                on_event(&ScriptLifecycleEvent::ContainmentEmpty {
                    schema_version: LIFECYCLE_EVENT_SCHEMA,
                    confidence: ContainmentConfidence::KernelEnforced,
                })?;
                return Ok(ScriptRecoveryOutcome::ContainmentTerminated);
            }
            cgroup.signal_members(libc::SIGTERM)?;
            thread::sleep(CONTROL_POLL_INTERVAL);
        }
        on_event(&ScriptLifecycleEvent::ForcedTerminationRequested {
            schema_version: LIFECYCLE_EVENT_SCHEMA,
            reason: TerminationReason::Recovery,
        })?;
        cgroup.kill_all()?;
        let kill_deadline = Instant::now() + KILL_GRACE;
        while Instant::now() < kill_deadline {
            if !cgroup.populated()? {
                on_event(&ScriptLifecycleEvent::ContainmentEmpty {
                    schema_version: LIFECYCLE_EVENT_SCHEMA,
                    confidence: ContainmentConfidence::KernelEnforced,
                })?;
                return Ok(ScriptRecoveryOutcome::ContainmentTerminated);
            }
            cgroup.kill_all()?;
            thread::sleep(CONTROL_POLL_INTERVAL);
        }
        Ok(ScriptRecoveryOutcome::ManualRecoveryRequired(
            "the verified Linux cgroup remained populated after cgroup.kill/SIGKILL; containment emptiness cannot be proved"
                .to_string(),
        ))
    }

    pub(super) fn cleanup_cgroup(
        request: &ScriptRecoveryRequest,
    ) -> Result<(), ScriptSupervisorError> {
        let identity = request.descriptor.cgroup.as_ref().ok_or_else(|| {
            ScriptSupervisorError::Protocol(
                "Linux cgroup containment descriptor omitted its cgroup identity".to_string(),
            )
        })?;
        match CgroupLeaf::open_existing(identity, &request.token) {
            Ok(cgroup) => cgroup.cleanup().map_err(ScriptSupervisorError::Io),
            Err(ScriptSupervisorError::Io(error))
                if error.raw_os_error() == Some(libc::ENOENT) => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn current_cgroup_directory() -> io::Result<PathBuf> {
        let cgroup = fs::read_to_string("/proc/self/cgroup")?;
        let unified = cgroup
            .lines()
            .find_map(|line| line.strip_prefix("0::"))
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no cgroup-v2 membership"))?;
        let mountinfo = fs::read_to_string("/proc/self/mountinfo")?;
        for line in mountinfo.lines() {
            let Some((left, right)) = line.split_once(" - ") else {
                continue;
            };
            if right.split_whitespace().next() != Some("cgroup2") {
                continue;
            }
            let fields: Vec<&str> = left.split_whitespace().collect();
            if fields.len() < 5 {
                continue;
            }
            let mount_root = unescape_mountinfo(fields[3]);
            let mount_point = unescape_mountinfo(fields[4]);
            let unified_path = Path::new(unified);
            let relative = if mount_root == Path::new("/") {
                unified_path.strip_prefix("/").unwrap_or(unified_path)
            } else {
                unified_path.strip_prefix(&mount_root).map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "cgroup membership is outside the cgroup2 mount root",
                    )
                })?
            };
            return Ok(mount_point.join(relative));
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "cgroup-v2 mount not found",
        ))
    }

    pub(super) fn unescape_mountinfo(value: &str) -> PathBuf {
        let bytes = value.as_bytes();
        let mut output = Vec::with_capacity(bytes.len());
        let mut index = 0;
        while index < bytes.len() {
            if bytes[index] == b'\\' && index + 3 < bytes.len() {
                let octal = &bytes[index + 1..index + 4];
                if octal.iter().all(|byte| matches!(byte, b'0'..=b'7')) {
                    let decoded = u16::from(octal[0] - b'0') * 64
                        + u16::from(octal[1] - b'0') * 8
                        + u16::from(octal[2] - b'0');
                    if let Ok(decoded) = u8::try_from(decoded) {
                        output.push(decoded);
                        index += 4;
                        continue;
                    }
                }
            }
            output.push(bytes[index]);
            index += 1;
        }
        PathBuf::from(OsString::from_vec(output))
    }

    fn open_directory_no_follow(path: &Path) -> io::Result<File> {
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
    }

    fn openat_directory(parent: RawFd, name: &OsString) -> io::Result<File> {
        let name = cstring(name)?;
        let fd = unsafe {
            libc::openat(
                parent,
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd >= 0 {
            Ok(unsafe { File::from_raw_fd(fd) })
        } else {
            Err(io::Error::last_os_error())
        }
    }

    fn read_control(directory: RawFd, name: &str) -> io::Result<String> {
        let name = CString::new(name).expect("static cgroup control name");
        let fd = unsafe {
            libc::openat(
                directory,
                name.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut file = unsafe { File::from_raw_fd(fd) };
        let mut text = String::new();
        file.read_to_string(&mut text)?;
        Ok(text)
    }

    fn write_control(directory: RawFd, name: &str, bytes: &[u8]) -> io::Result<()> {
        let name = CString::new(name).expect("static cgroup control name");
        let fd = unsafe {
            libc::openat(
                directory,
                name.as_ptr(),
                libc::O_WRONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut file = unsafe { File::from_raw_fd(fd) };
        file.write_all(bytes)
    }

    fn cstring(value: &OsString) -> io::Result<CString> {
        CString::new(value.as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "name contains NUL"))
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use std::mem::MaybeUninit;

    const PROC_PIDTBSDINFO: i32 = 3;
    const EVFILT_PROC: i16 = -5;
    const EV_ADD: u16 = 0x0001;
    const EV_ENABLE: u16 = 0x0004;
    const EV_CLEAR: u16 = 0x0020;
    const NOTE_EXIT: u32 = 0x8000_0000;
    const NOTE_FORK: u32 = 0x4000_0000;
    const NOTE_EXEC: u32 = 0x2000_0000;

    #[repr(C)]
    struct ProcBsdInfo {
        pbi_flags: u32,
        pbi_status: u32,
        pbi_xstatus: u32,
        pbi_pid: u32,
        pbi_ppid: u32,
        pbi_uid: u32,
        pbi_gid: u32,
        pbi_ruid: u32,
        pbi_rgid: u32,
        pbi_svuid: u32,
        pbi_svgid: u32,
        rfu_1: u32,
        pbi_comm: [libc::c_char; 16],
        pbi_name: [libc::c_char; 32],
        pbi_nfiles: u32,
        pbi_pgid: u32,
        pbi_pjobc: u32,
        e_tdev: u32,
        e_tpgid: u32,
        pbi_nice: i32,
        pbi_start_tvsec: u64,
        pbi_start_tvusec: u64,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    pub(super) struct ProcessIdentity {
        pub(super) pid: i32,
        pub(super) start_sec: u64,
        pub(super) start_usec: u64,
    }

    #[link(name = "proc")]
    extern "C" {
        fn proc_listallpids(buffer: *mut libc::c_void, buffersize: i32) -> i32;
        fn proc_listchildpids(
            ppid: i32,
            buffer: *mut libc::c_void,
            buffersize: i32,
        ) -> i32;
        fn proc_listpgrppids(
            pgrp: i32,
            buffer: *mut libc::c_void,
            buffersize: i32,
        ) -> i32;
        fn proc_pidinfo(
            pid: i32,
            flavor: i32,
            arg: u64,
            buffer: *mut libc::c_void,
            buffersize: i32,
        ) -> i32;
    }

    extern "C" {
        fn gethostuuid(identifier: *mut libc::c_uchar, wait: *const libc::timespec) -> libc::c_int;
    }

    struct KqueueGuard(RawFd);

    impl Drop for KqueueGuard {
        fn drop(&mut self) {
            unsafe {
                libc::close(self.0);
            }
        }
    }

    pub(super) fn machine_identity() -> Option<String> {
        let mut identifier = [0_u8; 16];
        let wait = libc::timespec {
            tv_sec: 5,
            tv_nsec: 0,
        };
        let result = unsafe { gethostuuid(identifier.as_mut_ptr(), &wait) };
        if result != 0 || identifier.iter().all(|byte| *byte == 0) {
            return None;
        }
        Some(
            identifier
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>(),
        )
    }

    pub(super) fn boot_identity() -> Option<String> {
        let name = std::ffi::CString::new("kern.boottime").ok()?;
        let mut value = libc::timeval { tv_sec: 0, tv_usec: 0 };
        let mut length = std::mem::size_of::<libc::timeval>();
        let result = unsafe {
            libc::sysctlbyname(
                name.as_ptr(),
                (&mut value as *mut libc::timeval).cast(),
                &mut length,
                std::ptr::null_mut(),
                0,
            )
        };
        if result == 0 && length == std::mem::size_of::<libc::timeval>() {
            Some(format!("{}:{}", value.tv_sec, value.tv_usec))
        } else {
            None
        }
    }

    fn stable_identity(identity: ProcessIdentity) -> StableProcessIdentity {
        StableProcessIdentity {
            pid: u32::try_from(identity.pid).unwrap_or(0),
            start_identity: format!("{}:{}", identity.start_sec, identity.start_usec),
        }
    }

    pub(super) fn supervisor_identity() -> Result<StableProcessIdentity, ScriptSupervisorError> {
        let identity = process_identity(std::process::id() as i32)?.ok_or_else(|| {
            ScriptSupervisorError::Internal(
                "cannot obtain the macOS supervisor process identity".to_string(),
            )
        })?;
        Ok(stable_identity(identity))
    }

    fn descriptor(
        spec: &SupervisorSpec,
        root: ProcessIdentity,
    ) -> Result<ContainmentDescriptor, ScriptSupervisorError> {
        Ok(ContainmentDescriptor {
            schema_version: LIFECYCLE_EVENT_SCHEMA,
            token: spec.token.clone(),
            backend: ContainmentBackend::MacosSupervisor,
            confidence: ContainmentConfidence::ProcessTreeObserved,
            host: current_host_boot_identity(),
            supervisor: supervisor_identity()?,
            leader: stable_identity(root),
            runtime_directory: spec.runtime_identity,
            cgroup: None,
            session_id: Some(root.pid),
            warning: Some(
                "macOS has no unprivileged cgroup-equivalent kernel container. Tonepoet uses an exec-gated private session, kqueue NOTE_FORK/NOTE_EXEC/NOTE_EXIT observation, recursive libproc scans, and PID/start-time validation. Processes launched indirectly through launchd or another external broker are outside that observable domain"
                    .to_string(),
            ),
        })
    }

    fn observe_leader_exit(
        event_fd: RawFd,
        child: &mut Child,
        raw_wait_status: &mut Option<i32>,
    ) -> Result<(), ScriptSupervisorError> {
        if raw_wait_status.is_none() {
            if let Some(status) = child.try_wait()? {
                let raw = raw_status(status);
                *raw_wait_status = Some(raw);
                emit_lifecycle_best_effort(
                    event_fd,
                    &ScriptLifecycleEvent::LeaderExited {
                        schema_version: LIFECYCLE_EVENT_SCHEMA,
                        raw_wait_status: raw,
                    },
                );
            }
        }
        Ok(())
    }

    fn complete_leader_wait(
        event_fd: RawFd,
        child: &mut Child,
        raw_wait_status: Option<i32>,
    ) -> Result<i32, ScriptSupervisorError> {
        if let Some(raw) = raw_wait_status {
            return Ok(raw);
        }
        let raw = raw_status(child.wait()?);
        emit_lifecycle_best_effort(
            event_fd,
            &ScriptLifecycleEvent::LeaderExited {
                schema_version: LIFECYCLE_EVENT_SCHEMA,
                raw_wait_status: raw,
            },
        );
        Ok(raw)
    }

    fn emit_empty(event_fd: RawFd) -> Result<(), ScriptSupervisorError> {
        emit_lifecycle_best_effort(
            event_fd,
            &ScriptLifecycleEvent::ContainmentEmpty {
                schema_version: LIFECYCLE_EVENT_SCHEMA,
                confidence: ContainmentConfidence::ProcessTreeObserved,
            },
        );
        Ok(())
    }

    pub(super) fn run(
        spec: &SupervisorSpec,
        control_fd: RawFd,
        event_fd: RawFd,
        script_fd: RawFd,
    ) -> Result<SupervisorResult, ScriptSupervisorError> {
        let kqueue = unsafe { libc::kqueue() };
        if kqueue < 0 {
            return Err(io::Error::last_os_error().into());
        }
        let kqueue_guard = KqueueGuard(kqueue);
        set_close_on_exec(kqueue_guard.0)?;
        let (mut child, launch_channel) = spawn_launcher(None, script_fd, || {
            if unsafe { libc::setsid() } < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        })?;
        let root = match process_identity(child.id() as i32) {
            Ok(Some(root)) => root,
            Ok(None) => {
                let cleanup = emergency_kill_child_group(&mut child);
                return Err(supervision_error_with_cleanup(
                    ScriptSupervisorError::Internal(
                        "script launcher vanished before macOS tracking was armed".to_string(),
                    ),
                    cleanup,
                ));
            }
            Err(error) => {
                let cleanup = emergency_kill_child_group(&mut child);
                return Err(supervision_error_with_cleanup(error.into(), cleanup));
            }
        };
        if let Err(error) = register_process(kqueue_guard.0, root.pid) {
            let cleanup = emergency_kill_child_group(&mut child);
            return Err(supervision_error_with_cleanup(error.into(), cleanup));
        }
        let mut tracked = BTreeSet::from([root]);
        let mut launch_channel = launch_channel;
        wait_launcher_ready(&mut launch_channel)?;
        let descriptor = descriptor(spec, root)?;
        if emit_lifecycle_event(
            event_fd,
            &ScriptLifecycleEvent::ContainmentPrepared {
                schema_version: LIFECYCLE_EVENT_SCHEMA,
                descriptor: descriptor.clone(),
            },
        )? != LifecycleAcknowledgement::Acknowledged
        {
            let mut raw_wait_status = None;
            let termination = terminate_tracked(
                event_fd,
                TerminationReason::ParentDisconnected,
                kqueue_guard.0,
                root.pid,
                &mut child,
                &mut tracked,
                &mut raw_wait_status,
            )?;
            let raw_wait_status = complete_leader_wait(event_fd, &mut child, raw_wait_status)?;
            return Ok(SupervisorResult {
                schema_version: SUPERVISOR_RESULT_SCHEMA,
                token: spec.token.clone(),
                raw_wait_status: Some(raw_wait_status),
                timed_out: false,
                cancelled: true,
                script_released: false,
                termination: Some(termination),
                descriptor: Some(descriptor),
                containment_empty: true,
                background_descendants: false,
                internal_error: None,
            });
        }
        if let Some(control) = poll_control(control_fd)? {
            let reason = control_reason(control).unwrap_or(TerminationReason::ParentDisconnected);
            let mut raw_wait_status = None;
            let termination = terminate_tracked(
                event_fd,
                reason,
                kqueue_guard.0,
                root.pid,
                &mut child,
                &mut tracked,
                &mut raw_wait_status,
            )?;
            let raw_wait_status = complete_leader_wait(event_fd, &mut child, raw_wait_status)?;
            return Ok(SupervisorResult {
                schema_version: SUPERVISOR_RESULT_SCHEMA,
                token: spec.token.clone(),
                raw_wait_status: Some(raw_wait_status),
                timed_out: reason == TerminationReason::Timeout,
                cancelled: matches!(
                    reason,
                    TerminationReason::Cancellation | TerminationReason::ParentDisconnected
                ),
                script_released: false,
                termination: Some(termination),
                descriptor: Some(descriptor),
                containment_empty: true,
                background_descendants: false,
                internal_error: None,
            });
        }

        release_launcher(launch_channel, spec)?;
        emit_lifecycle_best_effort(
            event_fd,
            &ScriptLifecycleEvent::UserCodeReleased {
                schema_version: LIFECYCLE_EVENT_SCHEMA,
                leader: stable_identity(root),
            },
        );
        let started = Instant::now();
        let timeout = Duration::from_millis(spec.timeout_millis);
        let mut raw_wait_status = None;
        let mut timed_out = false;
        let mut cancelled = false;
        let mut background_descendants = false;
        let mut termination = None;
        let mut leader_exited_at: Option<Instant> = None;
        loop {
            observe_leader_exit(event_fd, &mut child, &mut raw_wait_status)?;
            drain_kqueue(kqueue_guard.0)?;
            refresh_process_tree(kqueue_guard.0, root.pid, &mut tracked)?;
            tracked.retain(|identity| process_matches(*identity));
            if raw_wait_status.is_some() && tracked.is_empty() {
                emit_empty(event_fd)?;
                break;
            }
            if raw_wait_status.is_some() {
                let exited_at = leader_exited_at.get_or_insert_with(Instant::now);
                if exited_at.elapsed() >= BACKGROUND_EXIT_GRACE {
                    background_descendants = true;
                    termination = Some(terminate_tracked(
                        event_fd,
                        TerminationReason::LeaderExitedWithDescendants,
                        kqueue_guard.0,
                        root.pid,
                        &mut child,
                        &mut tracked,
                        &mut raw_wait_status,
                    )?);
                    break;
                }
            }
            if let Some(control) = poll_control(control_fd)? {
                let reason = control_reason(control).unwrap_or(TerminationReason::ParentDisconnected);
                timed_out = reason == TerminationReason::Timeout;
                cancelled = matches!(
                    reason,
                    TerminationReason::Cancellation | TerminationReason::ParentDisconnected
                );
                termination = Some(terminate_tracked(
                    event_fd,
                    reason,
                    kqueue_guard.0,
                    root.pid,
                    &mut child,
                    &mut tracked,
                    &mut raw_wait_status,
                )?);
                break;
            }
            if started.elapsed() >= timeout {
                timed_out = true;
                termination = Some(terminate_tracked(
                    event_fd,
                    TerminationReason::Timeout,
                    kqueue_guard.0,
                    root.pid,
                    &mut child,
                    &mut tracked,
                    &mut raw_wait_status,
                )?);
                break;
            }
            thread::sleep(CONTROL_POLL_INTERVAL);
        }
        let raw_wait_status = complete_leader_wait(event_fd, &mut child, raw_wait_status)?;
        Ok(SupervisorResult {
            schema_version: SUPERVISOR_RESULT_SCHEMA,
            token: spec.token.clone(),
            raw_wait_status: Some(raw_wait_status),
            timed_out,
            cancelled,
            script_released: true,
            termination,
            descriptor: Some(descriptor),
            containment_empty: true,
            background_descendants,
            internal_error: None,
        })
    }

    fn clear_errno() {
        unsafe {
            *libc::__error() = 0;
        }
    }

    fn current_errno() -> i32 {
        unsafe { *libc::__error() }
    }

    pub(super) fn process_identity(pid: i32) -> io::Result<Option<ProcessIdentity>> {
        clear_errno();
        let mut info = MaybeUninit::<ProcBsdInfo>::uninit();
        let size = i32::try_from(std::mem::size_of::<ProcBsdInfo>()).unwrap_or(i32::MAX);
        let returned = unsafe {
            proc_pidinfo(
                pid,
                PROC_PIDTBSDINFO,
                0,
                info.as_mut_ptr().cast(),
                size,
            )
        };
        if returned == 0 {
            let proc_error = io::Error::last_os_error();
            if proc_error.raw_os_error() == Some(libc::ESRCH) {
                return Ok(None);
            }

            // libproc does not consistently set errno when a PID vanishes.
            // Distinguish proven absence from an observation failure with a
            // second kernel query. EPERM proves the process exists but remains
            // unobservable, which must fail closed rather than look stale.
            clear_errno();
            let kill_result = unsafe { libc::kill(pid, 0) };
            if kill_result == 0 || current_errno() == libc::EPERM {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!(
                        "proc_pidinfo could not observe live macOS process {pid}: {proc_error}"
                    ),
                ));
            }
            let kill_error = io::Error::last_os_error();
            if kill_error.raw_os_error() == Some(libc::ESRCH) {
                return Ok(None);
            }
            return Err(if proc_error.raw_os_error() == Some(0) {
                kill_error
            } else {
                proc_error
            });
        }
        if returned != size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "proc_pidinfo returned a truncated proc_bsdinfo",
            ));
        }
        let info = unsafe { info.assume_init() };
        Ok(Some(ProcessIdentity {
            pid,
            start_sec: info.pbi_start_tvsec,
            start_usec: info.pbi_start_tvusec,
        }))
    }

    fn process_matches(expected: ProcessIdentity) -> bool {
        process_identity(expected.pid)
            .ok()
            .flatten()
            .map(|observed| observed == expected)
            .unwrap_or(false)
    }

    pub(super) fn stable_process_matches(
        expected: &StableProcessIdentity,
    ) -> Result<bool, ScriptSupervisorError> {
        let pid = i32::try_from(expected.pid).map_err(|_| {
            ScriptSupervisorError::Protocol("recorded macOS PID is out of range".to_string())
        })?;
        let (start_sec, start_usec) = expected.start_identity.split_once(':').ok_or_else(|| {
            ScriptSupervisorError::Protocol(
                "recorded macOS process start identity is malformed".to_string(),
            )
        })?;
        let start_sec = start_sec.parse::<u64>().map_err(|_| {
            ScriptSupervisorError::Protocol(
                "recorded macOS process start seconds are malformed".to_string(),
            )
        })?;
        let start_usec = start_usec.parse::<u64>().map_err(|_| {
            ScriptSupervisorError::Protocol(
                "recorded macOS process start microseconds are malformed".to_string(),
            )
        })?;
        Ok(process_identity(pid)?
            == Some(ProcessIdentity {
                pid,
                start_sec,
                start_usec,
            }))
    }

    fn register_process(kqueue: RawFd, pid: i32) -> io::Result<()> {
        let change = libc::kevent {
            ident: pid as usize,
            filter: EVFILT_PROC,
            flags: EV_ADD | EV_ENABLE | EV_CLEAR,
            fflags: NOTE_EXIT | NOTE_FORK | NOTE_EXEC,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        let result = unsafe {
            libc::kevent(
                kqueue,
                &change,
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        };
        if result == 0 {
            Ok(())
        } else {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                Ok(())
            } else {
                Err(error)
            }
        }
    }

    fn drain_kqueue(kqueue: RawFd) -> io::Result<()> {
        let mut events: [libc::kevent; 64] = std::array::from_fn(|_| libc::kevent {
            ident: 0,
            filter: 0,
            flags: 0,
            fflags: 0,
            data: 0,
            udata: std::ptr::null_mut(),
        });
        let timeout = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        loop {
            let count = unsafe {
                libc::kevent(
                    kqueue,
                    std::ptr::null(),
                    0,
                    events.as_mut_ptr(),
                    events.len() as i32,
                    &timeout,
                )
            };
            if count > 0 {
                if count < events.len() as i32 {
                    return Ok(());
                }
                continue;
            }
            if count == 0 {
                return Ok(());
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(error);
        }
    }

    fn refresh_process_tree(
        kqueue: RawFd,
        root_pgid: i32,
        tracked: &mut BTreeSet<ProcessIdentity>,
    ) -> io::Result<()> {
        let mut candidate_pids = BTreeSet::new();
        for identity in tracked.iter().copied().collect::<Vec<_>>() {
            candidate_pids.extend(list_child_pids(identity.pid)?);
        }
        candidate_pids.extend(list_group_pids(root_pgid)?);

        let all = list_all_process_info()?;
        let mut children = BTreeMap::<i32, Vec<i32>>::new();
        for (pid, parent, _) in &all {
            children.entry(*parent).or_default().push(*pid);
        }
        let mut queue: VecDeque<i32> = tracked.iter().map(|entry| entry.pid).collect();
        let mut visited: BTreeSet<i32> = queue.iter().copied().collect();
        while let Some(parent) = queue.pop_front() {
            if let Some(entries) = children.get(&parent) {
                for pid in entries {
                    if visited.insert(*pid) {
                        candidate_pids.insert(*pid);
                        queue.push_back(*pid);
                    }
                }
            }
        }

        for pid in candidate_pids {
            if let Some(identity) = process_identity(pid)? {
                if tracked.insert(identity) {
                    register_process(kqueue, pid)?;
                }
            }
        }
        Ok(())
    }

    fn list_child_pids(parent: i32) -> io::Result<Vec<i32>> {
        list_pid_buffer(|buffer, bytes| unsafe { proc_listchildpids(parent, buffer, bytes) })
    }

    fn list_group_pids(group: i32) -> io::Result<Vec<i32>> {
        list_pid_buffer(|buffer, bytes| unsafe { proc_listpgrppids(group, buffer, bytes) })
    }

    fn list_pid_buffer(call: impl Fn(*mut libc::c_void, i32) -> i32) -> io::Result<Vec<i32>> {
        let mut capacity = 256_usize;
        loop {
            let mut pids = vec![0_i32; capacity];
            let bytes = i32::try_from(pids.len() * std::mem::size_of::<i32>())
                .unwrap_or(i32::MAX);
            clear_errno();
            let returned = call(pids.as_mut_ptr().cast(), bytes);
            if returned < 0 || (returned == 0 && current_errno() != 0) {
                return Err(io::Error::last_os_error());
            }
            let count = returned as usize;
            if count < pids.len() {
                pids.truncate(count);
                pids.retain(|pid| *pid > 1);
                return Ok(pids);
            }
            capacity = capacity.saturating_mul(2);
            if capacity > 1_048_576 {
                return Err(io::Error::new(
                    io::ErrorKind::OutOfMemory,
                    "libproc process list exceeded the safety bound",
                ));
            }
        }
    }

    fn list_all_process_info() -> io::Result<Vec<(i32, i32, ProcessIdentity)>> {
        let pids = list_pid_buffer(|buffer, bytes| unsafe { proc_listallpids(buffer, bytes) })?;
        let mut result = Vec::new();
        for pid in pids {
            let mut info = MaybeUninit::<ProcBsdInfo>::uninit();
            let size = i32::try_from(std::mem::size_of::<ProcBsdInfo>()).unwrap_or(i32::MAX);
            let returned = unsafe {
                proc_pidinfo(
                    pid,
                    PROC_PIDTBSDINFO,
                    0,
                    info.as_mut_ptr().cast(),
                    size,
                )
            };
            if returned != size {
                continue;
            }
            let info = unsafe { info.assume_init() };
            result.push((
                pid,
                info.pbi_ppid as i32,
                ProcessIdentity {
                    pid,
                    start_sec: info.pbi_start_tvsec,
                    start_usec: info.pbi_start_tvusec,
                },
            ));
        }
        Ok(result)
    }

    fn terminate_tracked(
        event_fd: RawFd,
        reason: TerminationReason,
        kqueue: RawFd,
        root_pgid: i32,
        child: &mut Child,
        tracked: &mut BTreeSet<ProcessIdentity>,
        raw_wait_status: &mut Option<i32>,
    ) -> Result<TerminationSummary, ScriptSupervisorError> {
        let graceful_deadline_unix_millis = graceful_deadline_unix_millis();
        emit_lifecycle_best_effort(
            event_fd,
            &ScriptLifecycleEvent::TerminationRequested {
                schema_version: LIFECYCLE_EVENT_SCHEMA,
                reason,
                graceful_deadline_unix_millis,
            },
        );
        signal_process_group(child.id(), libc::SIGTERM);
        signal_tracked(tracked, libc::SIGTERM);
        let term_deadline = Instant::now() + TERM_GRACE;
        while Instant::now() < term_deadline {
            observe_leader_exit(event_fd, child, raw_wait_status)?;
            drain_kqueue(kqueue)?;
            refresh_process_tree(kqueue, root_pgid, tracked)?;
            tracked.retain(|identity| process_matches(*identity));
            if tracked.is_empty() {
                emit_empty(event_fd)?;
                return Ok(TerminationSummary {
                    reason,
                    graceful_deadline_unix_millis,
                    forced: false,
                });
            }
            signal_tracked(tracked, libc::SIGTERM);
            thread::sleep(CONTROL_POLL_INTERVAL);
        }

        emit_lifecycle_best_effort(
            event_fd,
            &ScriptLifecycleEvent::ForcedTerminationRequested {
                schema_version: LIFECYCLE_EVENT_SCHEMA,
                reason,
            },
        );
        signal_tracked(tracked, libc::SIGKILL);
        signal_process_group(child.id(), libc::SIGKILL);
        let kill_deadline = Instant::now() + KILL_GRACE;
        while Instant::now() < kill_deadline {
            observe_leader_exit(event_fd, child, raw_wait_status)?;
            drain_kqueue(kqueue)?;
            refresh_process_tree(kqueue, root_pgid, tracked)?;
            tracked.retain(|identity| process_matches(*identity));
            if tracked.is_empty() {
                emit_empty(event_fd)?;
                return Ok(TerminationSummary {
                    reason,
                    graceful_deadline_unix_millis,
                    forced: true,
                });
            }
            signal_tracked(tracked, libc::SIGKILL);
            thread::sleep(CONTROL_POLL_INTERVAL);
        }
        Err(ScriptSupervisorError::Internal(
            "macOS script containment could not be proven empty after SIGKILL; the journal must remain for manual recovery"
                .to_string(),
        ))
    }

    fn signal_tracked(tracked: &BTreeSet<ProcessIdentity>, signal: i32) {
        for identity in tracked.iter().rev() {
            if process_matches(*identity) {
                unsafe {
                    libc::kill(identity.pid, signal);
                }
            }
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    use super::linux::supervisor_identity;
    #[cfg(target_os = "macos")]
    use super::macos::supervisor_identity;

    #[test]
    fn cargo_test_helper_candidate_targets_unharnessed_sibling_binary() {
        let temp = tempfile::tempdir().expect("temp target root");
        let profile = temp.path().join("target").join("debug");
        let deps = profile.join("deps");
        std::fs::create_dir_all(profile.join(".fingerprint")).unwrap();
        std::fs::create_dir_all(&deps).unwrap();
        let current = deps.join("tonepoet-0123456789abcdef");
        let expected = profile.join(format!(
            "tonepoet{}",
            std::env::consts::EXE_SUFFIX
        ));
        assert_eq!(cargo_test_helper_candidate(&current), Some(expected));
        assert_eq!(cargo_test_helper_candidate(&profile.join("tonepoet")), None);
    }

    #[test]
    fn supervisor_spec_round_trips_without_shell_reinterpretation() {
        let spec = SupervisorSpec {
            schema_version: SUPERVISOR_RESULT_SCHEMA,
            token: "0123456789abcdef0123456789abcdef".to_string(),
            runtime_identity: RuntimeDirectoryIdentity { device: 1, inode: 2 },
            containment_preference: ContainmentPreference::Auto,
            script: PathBuf::from("/tmp/a script"),
            args: vec!["one two".to_string(), "$(not-shell)".to_string()],
            working_directory: PathBuf::from("/tmp"),
            environment: BTreeMap::from([(
                "TONEPOET_TITLE".to_string(),
                "O'Brien".to_string(),
            )]),
            timeout_millis: 1234,
        };
        let bytes = serde_json::to_vec(&spec).unwrap();
        let decoded: SupervisorSpec = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(decoded.args, spec.args);
        assert_eq!(decoded.environment, spec.environment);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn retained_descriptor_executes_reviewed_script_after_path_replacement() {
        use std::io::Read;
        use std::os::fd::{AsRawFd, FromRawFd};
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let script = temp.path().join("reviewed.sh");
        let replacement = temp.path().join("replacement.sh");
        fs::write(&script, b"#!/bin/sh\nprintf 'reviewed-code'\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let retained = File::open(&script).unwrap();

        fs::write(&replacement, b"#!/bin/sh\nprintf 'replacement-code'\n").unwrap();
        fs::set_permissions(&replacement, fs::Permissions::from_mode(0o755)).unwrap();
        fs::rename(&replacement, &script).unwrap();

        let mut pipe_fds = [-1_i32; 2];
        assert_eq!(unsafe { libc::pipe(pipe_fds.as_mut_ptr()) }, 0);
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0);
        if pid == 0 {
            unsafe {
                libc::close(pipe_fds[0]);
                if libc::dup2(pipe_fds[1], libc::STDOUT_FILENO) < 0 {
                    libc::_exit(120);
                }
                libc::close(pipe_fds[1]);
            }
            if clear_close_on_exec(retained.as_raw_fd()).is_err() {
                unsafe { libc::_exit(121) };
            }
            let spec = SupervisorSpec {
                schema_version: SUPERVISOR_RESULT_SCHEMA,
                token: "0123456789abcdef0123456789abcdef".to_string(),
                runtime_identity: RuntimeDirectoryIdentity { device: 1, inode: 2 },
                containment_preference: ContainmentPreference::Auto,
                script: script.clone(),
                args: Vec::new(),
                working_directory: temp.path().to_path_buf(),
                environment: BTreeMap::new(),
                timeout_millis: 1_000,
            };
            let _ = exec_retained_script(&spec, retained.as_raw_fd());
            unsafe { libc::_exit(122) };
        }

        unsafe { libc::close(pipe_fds[1]) };
        let mut output = Vec::new();
        let mut reader = unsafe { File::from_raw_fd(pipe_fds[0]) };
        reader.read_to_end(&mut output).unwrap();
        let mut status = 0_i32;
        assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
        assert!(libc::WIFEXITED(status));
        assert_eq!(libc::WEXITSTATUS(status), 0);
        assert_eq!(output, b"reviewed-code");
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn post_album_environment_is_rebound_to_retained_cwd_after_parent_replacement() {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::MetadataExt;

        let temp = tempfile::tempdir().unwrap();
        let live_parent = temp.path().join("live-output");
        let retained_parent = temp.path().join("renamed-output");
        let original_album = live_parent.join("Album");
        fs::create_dir_all(&original_album).unwrap();
        let retained = File::open(&original_album).unwrap();

        fs::rename(&live_parent, &retained_parent).unwrap();
        let replacement_album = live_parent.join("Album");
        fs::create_dir_all(&replacement_album).unwrap();

        let pid = unsafe { libc::fork() };
        assert!(pid >= 0);
        if pid == 0 {
            if unsafe { libc::fchdir(retained.as_raw_fd()) } != 0 {
                unsafe { libc::_exit(120) };
            }
            let mut spec = SupervisorSpec {
                schema_version: SUPERVISOR_RESULT_SCHEMA,
                token: "0123456789abcdef0123456789abcdef".to_string(),
                runtime_identity: RuntimeDirectoryIdentity { device: 1, inode: 2 },
                containment_preference: ContainmentPreference::Auto,
                script: temp.path().join("unused.sh"),
                args: Vec::new(),
                working_directory: original_album.clone(),
                environment: BTreeMap::from([
                    ("TONEPOET_PHASE".to_string(), "post".to_string()),
                    (
                        "TONEPOET_ALBUM_DIR".to_string(),
                        original_album.to_string_lossy().to_string(),
                    ),
                ]),
                timeout_millis: 1_000,
            };
            if bind_post_album_environment_to_retained_cwd(&mut spec, &retained).is_err() {
                unsafe { libc::_exit(121) };
            }
            let Some(exported) = spec.environment.get("TONEPOET_ALBUM_DIR") else {
                unsafe { libc::_exit(122) };
            };
            let Ok(exported_metadata) = fs::metadata(exported) else {
                unsafe { libc::_exit(123) };
            };
            let retained_metadata = retained.metadata().unwrap();
            let replacement_metadata = fs::metadata(&replacement_album).unwrap();
            let identifies_retained = exported_metadata.dev() == retained_metadata.dev()
                && exported_metadata.ino() == retained_metadata.ino();
            let identifies_replacement = exported_metadata.dev() == replacement_metadata.dev()
                && exported_metadata.ino() == replacement_metadata.ino();
            unsafe {
                libc::_exit(if identifies_retained && !identifies_replacement {
                    0
                } else {
                    124
                })
            };
        }

        let mut status = 0_i32;
        assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
        assert!(libc::WIFEXITED(status));
        assert_eq!(libc::WEXITSTATUS(status), 0);
    }

    #[test]
    fn containment_tokens_are_strictly_validated() {
        assert!(valid_token("0123456789abcdef0123456789abcdef"));
        assert!(!valid_token("../escape"));
        assert!(!valid_token("0123456789abcdef0123456789abcdeg"));
    }

    #[test]
    fn internal_error_result_cannot_smuggle_terminal_progress() {
        let token = "0123456789abcdef0123456789abcdef".to_string();
        let mut result = SupervisorResult::internal(token.clone(), "injected failure");
        result.containment_empty = true;
        assert!(matches!(
            validate_supervisor_result(&result, &token),
            Err(ScriptSupervisorError::Protocol(_))
        ));
    }

    #[test]
    fn timeout_conversion_saturates_instead_of_wrapping() {
        assert_eq!(duration_millis_u64(Duration::from_millis(9)), 9);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn item_supervisor_fd_transfer_is_cloexec_and_survives_sender_drop() {
        use std::io::{Read, Seek, SeekFrom, Write};
        use std::os::fd::AsRawFd;

        let temp = tempfile::tempfile().unwrap();
        let mut original = temp;
        original.write_all(b"lease-anchor").unwrap();
        original.seek(SeekFrom::Start(0)).unwrap();
        let (sender, receiver) = UnixStream::pair().unwrap();

        send_item_request(&sender, ITEM_REQUEST_LEASE, &[original.as_raw_fd()]).unwrap();
        let (tag, mut files) = receive_item_request(&receiver)
            .unwrap()
            .expect("one item-supervisor request");
        assert_eq!(tag, ITEM_REQUEST_LEASE);
        assert_eq!(files.len(), 1);

        let received = files.pop().unwrap();
        let fd_flags = unsafe { libc::fcntl(received.as_raw_fd(), libc::F_GETFD) };
        assert!(fd_flags >= 0);
        assert_ne!(fd_flags & libc::FD_CLOEXEC, 0);

        // The received descriptor is an independent reference to the same open
        // file description. Dropping the sender's File cannot invalidate the
        // supervisor's lifetime hold.
        drop(original);
        let mut received = received;
        received.seek(SeekFrom::Start(0)).unwrap();
        let mut body = String::new();
        received.read_to_string(&mut body).unwrap();
        assert_eq!(body, "lease-anchor");
    }

    #[test]
    fn launcher_ready_acknowledgement_precedes_spec_release() {
        let (mut supervisor, mut launcher) = UnixStream::pair().unwrap();
        let writer = thread::spawn(move || launcher.write_all(&[LAUNCHER_READY]).unwrap());
        wait_launcher_ready(&mut supervisor).unwrap();
        writer.join().unwrap();
    }

    #[test]
    fn launcher_ready_rejects_foreign_protocol_bytes() {
        let (mut supervisor, mut launcher) = UnixStream::pair().unwrap();
        let writer = thread::spawn(move || launcher.write_all(b"X").unwrap());
        assert!(matches!(
            wait_launcher_ready(&mut supervisor),
            Err(ScriptSupervisorError::Protocol(_))
        ));
        writer.join().unwrap();
    }

    #[test]
    fn tail_reader_stops_even_when_a_foreign_writer_keeps_the_pipe_open() {
        let (reader, mut writer) = UnixStream::pair().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let handle = spawn_tail_reader(reader, Arc::clone(&stop)).unwrap();
        writer.write_all(b"captured").unwrap();
        thread::sleep(Duration::from_millis(20));
        stop.store(true, Ordering::Release);
        let tail = handle.join().unwrap().unwrap();
        assert_eq!(tail.bytes, b"captured");
        assert_eq!(tail.terminal, OutputCaptureTerminal::Abandoned);
        drop(writer);
    }

    #[test]
    fn control_channel_eof_is_treated_as_cancellation() {
        let (supervisor, parent) = UnixStream::pair().unwrap();
        set_nonblocking(supervisor.as_raw_fd()).unwrap();
        drop(parent);
        assert_eq!(
            poll_control(supervisor.as_raw_fd()).unwrap(),
            Some(CONTROL_PARENT_GONE)
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_proc_stat_parser_handles_parentheses_in_comm() {
        let mut fields = vec!["S", "41", "77"];
        fields.extend(std::iter::repeat("0").take(16));
        fields.push("123456");
        let stat = format!("99 (a process ) name) {}", fields.join(" "));
        let parsed = linux::parse_proc_stat(99, &stat).unwrap();
        assert_eq!(parsed.identity.pid, 99);
        assert_eq!(parsed.identity.start_ticks, 123456);
        assert_eq!(parsed.parent, 41);
        assert_eq!(parsed.group, 77);
        assert_eq!(parsed.state, b'S');
    }

    #[test]
    fn stable_process_identity_rejects_pid_reuse_evidence() {
        let mut identity = supervisor_identity().expect("current process identity");
        // Model pid reuse with a numerically valid but different start tick:
        // a malformed identity is a hard protocol error, not a mismatch.
        let ticks: u64 = identity
            .start_identity
            .parse()
            .expect("linux start identity is tick-valued");
        identity.start_identity = (ticks + 1).to_string();
        #[cfg(target_os = "linux")]
        assert!(!linux::stable_process_matches(&identity).unwrap());
        #[cfg(target_os = "macos")]
        assert!(!macos::stable_process_matches(&identity).unwrap());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn recovery_distinguishes_a_validated_never_released_invocation() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let runtime = temp.path().join("runtime");
        fs::create_dir(&runtime).unwrap();
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
        let metadata = fs::metadata(&runtime).unwrap();
        let runtime_identity = RuntimeDirectoryIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        };
        let token = "0123456789abcdef0123456789abcdef".to_string();
        let process = supervisor_identity().expect("current process identity");
        #[cfg(target_os = "linux")]
        let backend = ContainmentBackend::LinuxSubreaper;
        #[cfg(target_os = "macos")]
        let backend = ContainmentBackend::MacosSupervisor;
        let descriptor = ContainmentDescriptor {
            schema_version: LIFECYCLE_EVENT_SCHEMA,
            token: token.clone(),
            backend,
            confidence: ContainmentConfidence::ProcessTreeObserved,
            host: current_host_boot_identity(),
            supervisor: process.clone(),
            leader: process,
            runtime_directory: runtime_identity,
            cgroup: None,
            session_id: None,
            warning: None,
        };
        let result = SupervisorResult {
            schema_version: SUPERVISOR_RESULT_SCHEMA,
            token: token.clone(),
            raw_wait_status: Some(9),
            timed_out: false,
            cancelled: true,
            script_released: false,
            termination: Some(TerminationSummary {
                reason: TerminationReason::ParentDisconnected,
                graceful_deadline_unix_millis: 123,
                forced: true,
            }),
            descriptor: Some(descriptor.clone()),
            containment_empty: true,
            background_descendants: false,
            internal_error: None,
        };
        let directory = open_private_runtime_directory(&runtime, runtime_identity).unwrap();
        write_private_json_new_at(directory.as_raw_fd(), RESULT_FILE_NAME, &result).unwrap();
        let recovered = recover_supervised(&ScriptRecoveryRequest {
            token,
            runtime_directory: runtime,
            descriptor,
        })
        .unwrap();
        assert_eq!(recovered, ScriptRecoveryOutcome::ExecutionNeverReleased);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn recovery_result_replays_termination_before_empty_proof() {
        let token = "0123456789abcdef0123456789abcdef".to_string();
        let process = supervisor_identity().expect("current process identity");
        #[cfg(target_os = "linux")]
        let backend = ContainmentBackend::LinuxSubreaper;
        #[cfg(target_os = "macos")]
        let backend = ContainmentBackend::MacosSupervisor;
        let descriptor = ContainmentDescriptor {
            schema_version: LIFECYCLE_EVENT_SCHEMA,
            token: token.clone(),
            backend,
            confidence: ContainmentConfidence::ProcessTreeObserved,
            host: current_host_boot_identity(),
            supervisor: process.clone(),
            leader: process,
            runtime_directory: RuntimeDirectoryIdentity { device: 1, inode: 2 },
            cgroup: None,
            session_id: None,
            warning: None,
        };
        let result = SupervisorResult {
            schema_version: SUPERVISOR_RESULT_SCHEMA,
            token,
            raw_wait_status: Some(9),
            timed_out: false,
            cancelled: true,
            script_released: true,
            termination: Some(TerminationSummary {
                reason: TerminationReason::ParentDisconnected,
                graceful_deadline_unix_millis: 123,
                forced: true,
            }),
            descriptor: Some(descriptor.clone()),
            containment_empty: true,
            background_descendants: false,
            internal_error: None,
        };
        validate_supervisor_result(&result, &result.token).unwrap();
        let mut events = Vec::new();
        replay_recovery_result_events(&result, &descriptor, &mut |event| {
            events.push(event.clone());
            Ok(())
        })
        .unwrap();
        assert!(matches!(
            events.as_slice(),
            [
                ScriptLifecycleEvent::TerminationRequested { .. },
                ScriptLifecycleEvent::ForcedTerminationRequested { .. },
                ScriptLifecycleEvent::LeaderExited { .. },
                ScriptLifecycleEvent::ContainmentEmpty { .. }
            ]
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_mountinfo_unescape_preserves_exact_bytes() {
        assert_eq!(
            linux::unescape_mountinfo("/sys/fs/cgroup/user\\040slice\\134name"),
            PathBuf::from("/sys/fs/cgroup/user slice\\name")
        );
        assert_eq!(
            linux::unescape_mountinfo("/sys/fs/cgroup/invalid\\777escape"),
            PathBuf::from("/sys/fs/cgroup/invalid\\777escape")
        );
    }
}
