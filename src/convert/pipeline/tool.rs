//! PR 1 — external-tool execution contract.
//!
//! Defines the closed set of tools the pipeline may invoke, the
//! command/output types, the `ToolRunner` trait, and a transcript-
//! backed stub runner for materializer/orchestrator unit tests.

use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant as StdInstant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tonepoet_pipeline::{CommandEnvironmentPolicy, Sha256Digest};

use super::errors::ToolRunnerError;
use super::types::SecretString;

/// Closed set of every external tool the pipeline invokes. No later
/// PR adds a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ToolBinary {
    SevenZip,
    Ffmpeg,
    Ffprobe,
    Sox,
    /// SSRC brick-wall resampler.
    Ssrc,
    Loudgain,
    Metaflac,
    /// Native FLAC command-line verifier/encoder.
    Flac,
    Opustags,
    Wvunpack,
    Wvtag,
    AtomicParsley,
    /// System tar used for archive repackaging.
    Tar,
    /// RAR archiver used for archive repackaging.
    Rar,
}

impl ToolBinary {
    /// The canonical system binary name used for PATH lookup and as
    /// the key into `ProcessorConfig.tool_paths`.
    pub fn canonical_name(&self) -> &'static str {
        match self {
            Self::SevenZip => "7z",
            Self::Ffmpeg => "ffmpeg",
            Self::Ffprobe => "ffprobe",
            Self::Sox => "sox",
            Self::Ssrc => "ssrc",
            Self::Loudgain => "loudgain",
            Self::Metaflac => "metaflac",
            Self::Flac => "flac",
            Self::Opustags => "opustags",
            Self::Wvunpack => "wvunpack",
            Self::Wvtag => "wvtag",
            Self::AtomicParsley => "AtomicParsley",
            Self::Tar => "tar",
            Self::Rar => "rar",
        }
    }

    /// Backward-compatible alias for older call sites.
    pub fn default_name(&self) -> &'static str {
        self.canonical_name()
    }
}

#[derive(Debug, Clone)]
pub struct EnvVar {
    pub key: String,
    pub value: SecretString,
    pub secret: bool,
}

/// One external command. `secret_args` indexes into `args` for
/// values that must be redacted in any log/transcript/record.
#[derive(Debug, Clone)]
pub struct ToolCommand {
    pub binary: ToolBinary,
    pub args: Vec<String>,
    pub secret_args: Vec<usize>,
    pub cwd: Option<PathBuf>,
    pub environment_policy: CommandEnvironmentPolicy,
    pub env: Vec<EnvVar>,
    pub timeout: Duration,
}

/// Exact executable authority for a command whose runtime binary identity is
/// part of a qualified policy. The runner must reject path or content drift
/// and execute this canonical path rather than performing a second PATH lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundToolExecutable {
    pub canonical_path: PathBuf,
    pub executable_sha256: Sha256Digest,
}

impl ToolCommand {
    /// Args with `secret_args` positions replaced by a redaction
    /// marker — safe to log, store in a `CommandRecord`, or print.
    pub fn sanitized_args(&self) -> Vec<String> {
        self.args
            .iter()
            .enumerate()
            .map(|(i, a)| {
                if self.secret_args.contains(&i) {
                    "<redacted>".to_string()
                } else {
                    a.clone()
                }
            })
            .collect()
    }

    /// Environment keys in execution order.
    pub fn env_keys(&self) -> Vec<String> {
        self.env.iter().map(|entry| entry.key.clone()).collect()
    }

    /// Sanitized explicit environment. Duplicate keys follow command
    /// semantics: the final value wins.
    pub fn sanitized_environment(&self) -> BTreeMap<String, String> {
        let mut environment = BTreeMap::new();
        for entry in &self.env {
            environment.insert(
                entry.key.clone(),
                if entry.secret {
                    "<redacted>".to_string()
                } else {
                    entry.value.expose().to_string()
                },
            );
        }
        environment
    }
}

/// Process termination — handles signal death and missing exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProcessExit {
    Code(i32),
    Signal(i32),
    Unknown,
}

/// Sanitized record of one command invocation. Carried on both
/// success (`ToolOutput`) and failure (`ToolRunnerError`). Never
/// contains a secret.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandRecord {
    /// User-facing planner description for this invocation, when the command
    /// originated from a `PlannedCommand`. Kept optional so older durable logs
    /// and non-planner tool calls remain backward-compatible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub binary: ToolBinary,
    pub sanitized_args: Vec<String>,
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub environment_policy: CommandEnvironmentPolicy,
    /// Sanitized explicit environment installed by the command.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub environment: BTreeMap<String, String>,
    pub env_keys: Vec<String>,
    pub exit: Option<ProcessExit>,
    pub stdout_tail: String,
    pub stderr_tail: String,
    pub elapsed: Duration,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOutput {
    pub exit: ProcessExit,
    /// Bounded to a 64 KiB tail.
    pub stdout_tail: String,
    /// Bounded to a 64 KiB tail.
    pub stderr_tail: String,
    pub elapsed: Duration,
    pub command: CommandRecord,
}

/// Result of a typed two-process pipeline whose producer stdout is connected
/// directly to the consumer stdin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolPipelineOutput {
    pub producer: ToolOutput,
    pub consumer: ToolOutput,
}

/// Pipeline failure plus any other stage records already available for durable
/// diagnostics.
#[derive(Debug)]
pub struct ToolPipelineError {
    pub error: ToolRunnerError,
    pub other_commands: Vec<CommandRecord>,
}

#[cfg(unix)]
/// A downstream failure can close the read end before arbitration observes it,
/// making the producer's SIGPIPE a consequence rather than the root failure.
/// Never apply this rule to peer cancellation or to ordinary producer errors.
fn consumer_failure_makes_producer_sigpipe_secondary(
    producer_error: &ToolRunnerError,
    consumer_error: &ToolRunnerError,
) -> bool {
    matches!(
        producer_error,
        ToolRunnerError::NonZeroExit {
            exit: ProcessExit::Signal(signal),
            ..
        } if *signal == libc::SIGPIPE
    ) && matches!(
        consumer_error,
        ToolRunnerError::Spawn { .. }
            | ToolRunnerError::Timeout { .. }
            | ToolRunnerError::Termination { .. }
            | ToolRunnerError::NonZeroExit { .. }
            | ToolRunnerError::Io(_)
    )
}

/// Maximum stdout/stderr tail any runner retains.
pub const TOOL_OUTPUT_TAIL_BYTES: usize = 64 * 1024;

#[async_trait]
pub trait ToolRunner: Send + Sync {
    async fn run(
        &self,
        cmd: ToolCommand,
        cancel: &CancellationToken,
    ) -> Result<ToolOutput, ToolRunnerError>;

    /// Execute a command through an exact path-and-content authority. A runner
    /// must explicitly implement this contract; the default fails closed rather
    /// than silently degrading to ordinary configured-path/PATH execution.
    async fn run_bound(
        &self,
        _cmd: ToolCommand,
        _executable: &BoundToolExecutable,
        _cancel: &CancellationToken,
    ) -> Result<ToolOutput, ToolRunnerError> {
        Err(ToolRunnerError::Io(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "tool runner does not implement exact bound executable execution",
        )))
    }

    /// Run a typed producer-consumer pipeline. Implementations must either
    /// provide real producer-stdout-to-consumer-stdin transport or reject the
    /// operation explicitly. Sequential execution is never a pipeline.
    async fn run_pipeline(
        &self,
        _producer: ToolCommand,
        _consumer: ToolCommand,
        _cancel: &CancellationToken,
    ) -> Result<ToolPipelineOutput, ToolPipelineError> {
        Err(ToolPipelineError {
            error: ToolRunnerError::UnsupportedPipeline,
            other_commands: Vec::new(),
        })
    }

    /// Return the detected version for an external binary, if available.
    ///
    /// Test runners and in-process tool implementations inherit the default
    /// `None`; real runners override this with best-effort, cached detection.
    fn tool_version(&self, _binary: ToolBinary) -> Option<String> {
        None
    }

    /// Return whether a binary can be resolved before an album starts doing
    /// irreversible or expensive work. Test/in-process runners default to
    /// available; the real runner performs an executable-path lookup.
    fn tool_available(&self, _binary: ToolBinary) -> bool {
        true
    }

    /// Return the exact executable path this runner will spawn for `binary`.
    /// Reference-policy attestation uses this to bind the probed executable to
    /// the executable that later commands actually run. Test/in-process
    /// runners may leave it unavailable when they cannot make that guarantee.
    fn resolved_tool_path(&self, _binary: ToolBinary) -> Option<PathBuf> {
        None
    }
}

// ===========================================================================
// Stub runner — PR 1
// ===========================================================================

/// Configured response for the next stub `run` call.
#[derive(Clone)]
enum StubResponse {
    Output(ToolOutput),
    Fail(String),
    Spawn,
}

/// Transcript-backed `ToolRunner` for unit tests. Never spawns a
/// process. Records every `ToolCommand` (sanitized) and returns
/// test-configured outputs.
pub struct StubToolRunner {
    transcript: Mutex<Vec<CommandRecord>>,
    responses: Mutex<Vec<StubResponse>>,
    default_exit: ProcessExit,
}

impl StubToolRunner {
    pub fn new() -> Self {
        Self {
            transcript: Mutex::new(Vec::new()),
            responses: Mutex::new(Vec::new()),
            default_exit: ProcessExit::Code(0),
        }
    }

    /// Queue a successful output for an upcoming `run` call.
    pub fn push_output(&self, output: ToolOutput) {
        self.responses
            .lock()
            .unwrap()
            .push(StubResponse::Output(output));
    }

    /// Queue a `NonZeroExit` failure for an upcoming `run` call.
    pub fn push_failure(&self, stderr: impl Into<String>) {
        self.responses
            .lock()
            .unwrap()
            .push(StubResponse::Fail(stderr.into()));
    }

    /// Queue a tool-spawn failure for an upcoming `run` call.
    pub fn push_spawn_failure(&self) {
        self.responses.lock().unwrap().push(StubResponse::Spawn);
    }

    /// The sanitized command transcript, in call order.
    pub fn transcript(&self) -> Vec<CommandRecord> {
        self.transcript.lock().unwrap().clone()
    }

    fn record(&self, cmd: &ToolCommand, exit: Option<ProcessExit>, stderr: &str) -> CommandRecord {
        CommandRecord {
            environment_policy: cmd.environment_policy,
            environment: cmd.sanitized_environment(),
            description: None,
            binary: cmd.binary,
            sanitized_args: cmd.sanitized_args(),
            cwd: cmd.cwd.clone(),
            env_keys: cmd.env_keys(),
            exit,
            stdout_tail: String::new(),
            stderr_tail: stderr.to_string(),
            elapsed: Duration::ZERO,
        }
    }
}

impl Default for StubToolRunner {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolRunner for StubToolRunner {
    async fn run(
        &self,
        cmd: ToolCommand,
        _cancel: &CancellationToken,
    ) -> Result<ToolOutput, ToolRunnerError> {
        let queued = {
            let mut r = self.responses.lock().unwrap();
            if r.is_empty() {
                None
            } else {
                Some(r.remove(0))
            }
        };
        match queued {
            Some(StubResponse::Spawn) => {
                let record = self.record(&cmd, None, "");
                self.transcript.lock().unwrap().push(record.clone());
                Err(ToolRunnerError::Spawn { command: record })
            }
            Some(StubResponse::Fail(stderr)) => {
                let record = self.record(&cmd, Some(ProcessExit::Code(1)), &stderr);
                self.transcript.lock().unwrap().push(record.clone());
                Err(ToolRunnerError::NonZeroExit {
                    exit: ProcessExit::Code(1),
                    stderr_tail: stderr,
                    command: record,
                })
            }
            Some(StubResponse::Output(mut output)) => {
                let record = self.record(&cmd, Some(output.exit), &output.stderr_tail);
                self.transcript.lock().unwrap().push(record.clone());
                output.command = record;
                Ok(output)
            }
            None => {
                // No configured response: a benign success.
                let record = self.record(&cmd, Some(self.default_exit), "");
                self.transcript.lock().unwrap().push(record.clone());
                Ok(ToolOutput {
                    exit: self.default_exit,
                    stdout_tail: String::new(),
                    stderr_tail: String::new(),
                    elapsed: Duration::ZERO,
                    command: record,
                })
            }
        }
    }
}

// ===========================================================================
// Real runner — PR 2
// ===========================================================================

/// Async child-process `ToolRunner`. Spawns real processes via tokio,
/// enforces per-command timeouts, honours cancellation tokens, captures
/// bounded stdout/stderr tails, and always produces sanitized
/// `CommandRecord`s on both success and failure.
pub struct RealToolRunner {
    tool_paths: HashMap<String, PathBuf>,
    version_cache: Arc<Mutex<HashMap<ToolBinary, String>>>,
    execution_item_id: Option<String>,
    execution_supervisor: Option<crate::convert::script_supervisor::ItemExecutionSupervisorClient>,
}

impl RealToolRunner {
    /// Create a runner. `tool_paths` keys are canonical binary names
    /// (e.g. `"ffmpeg"`, `"sox"`) matching `ToolBinary::default_name()`.
    /// An empty map means all tools are resolved from `$PATH`.
    pub fn new(tool_paths: HashMap<String, PathBuf>) -> Self {
        Self::with_version_cache(tool_paths, Arc::new(Mutex::new(HashMap::new())))
    }

    /// Create a runner with a caller-supplied per-session version cache.
    /// Production workers pass the same `Arc` here so first-use detection is
    /// shared across the session; tests and legacy call sites can keep using
    /// `RealToolRunner::new(tool_paths)` for an isolated empty cache.
    pub fn with_version_cache(
        tool_paths: HashMap<String, PathBuf>,
        version_cache: Arc<Mutex<HashMap<ToolBinary, String>>>,
    ) -> Self {
        Self {
            tool_paths,
            version_cache,
            execution_item_id: None,
            execution_supervisor: None,
        }
    }

    /// Bind this runner to one durable queue execution. Scheduled fan-out
    /// carries this explicit identity so task-local scope is never the only
    /// route to the per-item supervisor.
    pub fn with_execution_item(mut self, item_id: impl Into<String>) -> Self {
        let item_id = item_id.into();
        // Capture the typed process capability at runner construction. A bound
        // scheduled runner never relies on whatever Tokio task happens to poll
        // it later; missing authority remains a hard run-time error.
        self.execution_supervisor = crate::concurrency::runtime_item_supervisor(&item_id).ok();
        self.execution_item_id = Some(item_id);
        self
    }

    /// Resolve a `ToolBinary` to an executable path.
    ///
    /// 1. If `tool_paths` contains a custom override for the binary's
    ///    canonical name, use it.
    /// 2. For `SevenZip`, probe for `7zz` (faster) then `7z`.
    /// 3. Otherwise fall back to the canonical name on `$PATH`.
    pub(crate) fn resolve_binary(&self, binary: ToolBinary) -> PathBuf {
        let name = binary.default_name();
        if let Some(path) = self.tool_paths.get(name) {
            return path.clone();
        }
        if binary == ToolBinary::SevenZip {
            if let Some(bin) = crate::detect_7z_binary() {
                return PathBuf::from(bin);
            }
        }
        PathBuf::from(name)
    }

    fn tool_version_for_resolved_path(&self, binary: ToolBinary, path: &Path) -> Option<String> {
        let wait_started_at = StdInstant::now();

        loop {
            let should_probe = {
                let mut cache = self.version_cache.lock().ok()?;
                match cache.get(&binary) {
                    Some(cached) if cached == TOOL_VERSION_CACHE_IN_PROGRESS => false,
                    Some(cached) => return cached_version_to_option(cached),
                    None => {
                        cache.insert(binary, TOOL_VERSION_CACHE_IN_PROGRESS.to_string());
                        true
                    }
                }
            };

            if should_probe {
                // Do the potentially blocking probe outside the mutex so
                // unrelated workers can continue reading already-cached
                // versions. The in-progress marker above prevents concurrent
                // first-use callers for the same binary from spawning duplicate
                // probes; they wait for this owner to publish the cached
                // result. The publish guard fires on ALL exits — including a
                // panic mid-probe — so waiters can never be wedged behind a
                // permanently in-progress marker.
                struct PublishOnDrop<'a> {
                    cache: &'a Mutex<HashMap<ToolBinary, String>>,
                    binary: ToolBinary,
                    value: String,
                }
                impl Drop for PublishOnDrop<'_> {
                    fn drop(&mut self) {
                        if let Ok(mut cache) = self.cache.lock() {
                            cache.insert(self.binary, std::mem::take(&mut self.value));
                        }
                    }
                }
                let mut publish = PublishOnDrop {
                    cache: &self.version_cache,
                    binary,
                    value: String::new(),
                };
                let detected = detect_tool_version(binary, path);
                publish.value = detected.clone().unwrap_or_default();
                drop(publish);
                return detected;
            }

            if wait_started_at.elapsed() >= TOOL_VERSION_CACHE_WAIT_TIMEOUT {
                return None;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Build a sanitized `CommandRecord` from a `ToolCommand` and
    /// whatever exit / output info is available at the call site.
    fn build_record(
        cmd: &ToolCommand,
        exit: Option<ProcessExit>,
        stdout_tail: &str,
        stderr_tail: &str,
        elapsed: Duration,
    ) -> CommandRecord {
        CommandRecord {
            environment_policy: cmd.environment_policy,
            environment: cmd.sanitized_environment(),
            description: None,
            binary: cmd.binary,
            sanitized_args: cmd.sanitized_args(),
            cwd: cmd.cwd.clone(),
            env_keys: cmd.env_keys(),
            exit,
            stdout_tail: stdout_tail.to_string(),
            stderr_tail: stderr_tail.to_string(),
            elapsed,
        }
    }
}

const TOOL_VERSION_DETECTION_TIMEOUT: Duration = Duration::from_millis(100);
// Waiters must get the PUBLISHED answer for a once-per-binary probe rather
// than fabricating "no version" under load: subprocess spawn can exceed
// hundreds of milliseconds when the test suite (or a busy conversion) is
// fork-storming, and a waiter that gives up records wrong provenance. The
// probe publishes exactly once (unwind-safe), so this bound is a last-resort
// escape hatch, not an expected path.
const TOOL_VERSION_CACHE_WAIT_TIMEOUT: Duration = Duration::from_secs(5);
const TOOL_VERSION_CAPTURE_BYTES: u64 = 1024 * 1024;
static TOOL_VERSION_CAPTURE_COUNTER: AtomicU64 = AtomicU64::new(0);
const TOOL_VERSION_CACHE_IN_PROGRESS: &str = "\0tonepoet-tool-version-in-progress";

fn cached_version_to_option(cached: &str) -> Option<String> {
    if cached.is_empty() || cached == TOOL_VERSION_CACHE_IN_PROGRESS {
        None
    } else {
        Some(cached.to_string())
    }
}

fn version_command_args(binary: ToolBinary) -> &'static [&'static str] {
    match binary {
        ToolBinary::SevenZip | ToolBinary::Ssrc => &[],
        ToolBinary::Tar | ToolBinary::Rar => &["--version"],
        ToolBinary::Sox | ToolBinary::Opustags => &["--help"],
        // AtomicParsley emits its version banner on the zero-argument path;
        // `--version` is not its canonical identity probe.
        ToolBinary::AtomicParsley => &[],
        ToolBinary::Ffmpeg
        | ToolBinary::Ffprobe
        | ToolBinary::Loudgain
        | ToolBinary::Metaflac
        | ToolBinary::Flac
        | ToolBinary::Wvunpack
        | ToolBinary::Wvtag => &["--version"],
    }
}

fn normalize_version_token(token: &str) -> Option<String> {
    let trimmed = token
        .trim()
        .trim_start_matches(['v', 'V'])
        .trim_matches(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_'));
    if trimmed.is_empty() || !trimmed.chars().next()?.is_ascii_digit() {
        return None;
    }
    if !trimmed.chars().any(|c| c == '.') {
        return None;
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_')
    {
        return None;
    }
    Some(trimmed.to_string())
}

fn first_version_like_token(text: &str) -> Option<String> {
    text.split_whitespace().find_map(normalize_version_token)
}

fn first_version_after_marker(line: &str, marker: &str) -> Option<String> {
    let lower = line.to_ascii_lowercase();
    let marker = marker.to_ascii_lowercase();
    let start = lower.find(&marker)? + marker.len();
    first_version_like_token(&line[start..])
}

pub(crate) fn parse_tool_version_output(binary: ToolBinary, stdout: &str, stderr: &str) -> Option<String> {
    let combined = [stdout, stderr].join("\n");
    for line in combined.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let parsed = match binary {
            ToolBinary::Ffmpeg | ToolBinary::Ffprobe | ToolBinary::Ssrc | ToolBinary::Opustags => {
                first_version_after_marker(line, "version")
            }
            ToolBinary::Sox => first_version_after_marker(line, "SoX_ng")
                .or_else(|| first_version_after_marker(line, "SoX"))
                .or_else(|| first_version_after_marker(line, "version")),
            ToolBinary::SevenZip => first_version_after_marker(line, "7-Zip")
                .or_else(|| first_version_after_marker(line, "7z")),
            ToolBinary::Loudgain
            | ToolBinary::Metaflac
            | ToolBinary::Flac
            | ToolBinary::Wvunpack
            | ToolBinary::Wvtag
            | ToolBinary::AtomicParsley
            | ToolBinary::Tar
            | ToolBinary::Rar => first_version_like_token(line),
        };
        if parsed.is_some() {
            return parsed;
        }
    }
    None
}

fn unique_tool_version_capture_path(binary: ToolBinary, stream: &str) -> PathBuf {
    let counter = TOOL_VERSION_CAPTURE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let name = format!(
        "tool-version-{}-{}-{}-{}-{stream}.tmp",
        binary.canonical_name(),
        std::process::id(),
        timestamp,
        counter
    );
    std::env::temp_dir().join(name)
}

fn remove_version_probe_capture(path: &Path) {
    let _ = std::fs::remove_file(path);
}

fn read_version_probe_capture(path: &Path) -> Vec<u8> {
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let mut reader = file.take(TOOL_VERSION_CAPTURE_BYTES);
    let mut captured = Vec::new();
    let _ = reader.read_to_end(&mut captured);
    captured
}

fn wait_for_version_probe(child: &mut std::process::Child) -> bool {
    let deadline = StdInstant::now() + TOOL_VERSION_DETECTION_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => return true,
            Ok(None) => {
                if StdInstant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return false;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

/// Best-effort synchronous version detection for external tools.
///
/// The probe is intentionally bounded and never connects stdout/stderr to
/// pipes. Probe output is redirected to unique temp files, the child is killed
/// on timeout, and only a bounded amount of captured output is read back. This
/// avoids pipe backpressure for verbose `--help` output while keeping version
/// detection non-fatal for conversion.
pub(crate) fn detect_tool_version(binary: ToolBinary, path: &Path) -> Option<String> {
    let stdout_path = unique_tool_version_capture_path(binary, "stdout");
    let stderr_path = unique_tool_version_capture_path(binary, "stderr");

    let stdout_file = match std::fs::File::create(&stdout_path) {
        Ok(file) => file,
        Err(_) => return None,
    };
    let stderr_file = match std::fs::File::create(&stderr_path) {
        Ok(file) => file,
        Err(_) => {
            remove_version_probe_capture(&stdout_path);
            return None;
        }
    };

    let mut probe = std::process::Command::new(path);
    probe
        .args(version_command_args(binary))
        // Version probes are provenance helpers, not part of a command's
        // processing identity. Keep them deterministic and unable to observe
        // ambient configuration regardless of the caller's command policy.
        .env_clear()
        .env("LC_ALL", "C")
        // Same rule as every other subprocess (see the archive-hang fix): a
        // tool that reads stdin on a version-ish invocation must get EOF, not
        // an inherited terminal — a blocked probe wedges the version cache's
        // in-progress marker and stalls every waiter.
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(stderr_file));
    let spawn_result = probe.spawn();

    let mut child = match spawn_result {
        Ok(child) => child,
        Err(_) => {
            remove_version_probe_capture(&stdout_path);
            remove_version_probe_capture(&stderr_path);
            return None;
        }
    };

    let completed = wait_for_version_probe(&mut child);
    let stdout = read_version_probe_capture(&stdout_path);
    let stderr = read_version_probe_capture(&stderr_path);
    remove_version_probe_capture(&stdout_path);
    remove_version_probe_capture(&stderr_path);

    if !completed {
        return None;
    }

    let stdout = String::from_utf8_lossy(&stdout);
    let stderr = String::from_utf8_lossy(&stderr);
    parse_tool_version_output(binary, &stdout, &stderr)
}

/// Read an async reader to completion and return at most the last
/// `TOOL_OUTPUT_TAIL_BYTES` as a UTF-8 string (lossy).
async fn read_tail(mut reader: impl tokio::io::AsyncRead + Unpin) -> String {
    let mut tail = Vec::with_capacity(TOOL_OUTPUT_TAIL_BYTES);
    let mut buf = [0_u8; 8192];
    loop {
        let read = match reader.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };
        tail.extend_from_slice(&buf[..read]);
        if tail.len() > TOOL_OUTPUT_TAIL_BYTES {
            let excess = tail.len() - TOOL_OUTPUT_TAIL_BYTES;
            tail.drain(..excess);
        }
    }
    String::from_utf8_lossy(&tail).into_owned()
}

pub(crate) const TOOL_TERMINATION_TIMEOUT: Duration = Duration::from_secs(5);
const TOOL_PIPELINE_POLL_INTERVAL: Duration = Duration::from_millis(10);

pub(crate) fn apply_command_environment(
    process: &mut tokio::process::Command,
    command: &ToolCommand,
) {
    if command.environment_policy == CommandEnvironmentPolicy::ClearAndSet {
        process.env_clear();
    }
    for env_var in &command.env {
        process.env(&env_var.key, env_var.value.expose());
    }
}

pub(crate) async fn terminate_and_reap_child(
    child: &mut tokio::process::Child,
    label: &str,
) -> Result<std::process::ExitStatus, String> {
    let inspection_error = match child.try_wait() {
        Ok(Some(status)) => return Ok(status),
        Ok(None) => None,
        Err(error) => Some(error),
    };
    let kill_error = child.start_kill().err();
    match tokio::time::timeout(TOOL_TERMINATION_TIMEOUT, child.wait()).await {
        Ok(Ok(status)) => Ok(status),
        Ok(Err(error)) => Err(format!(
            "cannot reap {label}; inspection_error={inspection_error:?}; \
             kill_error={kill_error:?}; wait_error={error}"
        )),
        Err(_) => Err(format!(
            "{label} did not terminate and reap within {:?}; \
             inspection_error={inspection_error:?}; kill_error={kill_error:?}",
            TOOL_TERMINATION_TIMEOUT
        )),
    }
}

#[derive(Debug)]
struct PipelineTerminationFailure {
    message: String,
    producer_status: Option<std::process::ExitStatus>,
    consumer_status: Option<std::process::ExitStatus>,
}

async fn make_pipeline_terminal(
    producer: &mut tokio::process::Child,
    consumer: &mut tokio::process::Child,
    producer_status: Option<std::process::ExitStatus>,
    consumer_status: Option<std::process::ExitStatus>,
    reason: &str,
) -> Result<
    (std::process::ExitStatus, std::process::ExitStatus),
    PipelineTerminationFailure,
> {
    let producer_label = format!("pipeline producer {reason}");
    let consumer_label = format!("pipeline consumer {reason}");
    let producer_future = async {
        match producer_status {
            Some(status) => Ok(status),
            None => terminate_and_reap_child(producer, &producer_label).await,
        }
    };
    let consumer_future = async {
        match consumer_status {
            Some(status) => Ok(status),
            None => terminate_and_reap_child(consumer, &consumer_label).await,
        }
    };
    let (producer_result, consumer_result) = tokio::join!(producer_future, consumer_future);
    match (producer_result, consumer_result) {
        (Ok(producer_status), Ok(consumer_status)) => Ok((producer_status, consumer_status)),
        (producer_result, consumer_result) => {
            let producer_error = producer_result.as_ref().err().cloned();
            let consumer_error = consumer_result.as_ref().err().cloned();
            Err(PipelineTerminationFailure {
                message: format!(
                    "producer termination/reaping: {}; consumer termination/reaping: {}",
                    producer_error.as_deref().unwrap_or("completed"),
                    consumer_error.as_deref().unwrap_or("completed")
                ),
                producer_status: producer_result.ok(),
                consumer_status: consumer_result.ok(),
            })
        }
    }
}

async fn collect_tail_task(mut task: tokio::task::JoinHandle<String>) -> String {
    match tokio::time::timeout(TOOL_TERMINATION_TIMEOUT, &mut task).await {
        Ok(Ok(tail)) => tail,
        Ok(Err(_)) => String::new(),
        Err(_) => {
            task.abort();
            let _ = task.await;
            String::new()
        }
    }
}

/// Map a `std::process::ExitStatus` to a `ProcessExit`.
fn map_exit_status(status: std::process::ExitStatus) -> ProcessExit {
    if let Some(code) = status.code() {
        return ProcessExit::Code(code);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return ProcessExit::Signal(sig);
        }
    }
    ProcessExit::Unknown
}

fn usable_executable_file(candidate: &Path) -> bool {
    let Ok(metadata) = candidate.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(not(windows))]
fn path_search_candidates(directory: &Path, program: &Path) -> Vec<PathBuf> {
    vec![directory.join(program)]
}

#[cfg(windows)]
fn path_search_candidates(directory: &Path, program: &Path) -> Vec<PathBuf> {
    let base = directory.join(program);
    if program.extension().is_some() {
        return vec![base];
    }
    let path_ext = std::env::var_os("PATHEXT")
        .and_then(|value| value.into_string().ok())
        .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".to_string());
    let mut candidates = vec![base.clone()];
    for extension in path_ext.split(';').map(str::trim).filter(|value| !value.is_empty()) {
        let mut candidate = base.clone();
        candidate.set_extension(extension.trim_start_matches('.'));
        candidates.push(candidate);
    }
    candidates
}

fn resolve_executable_launch_path(path: &Path) -> Option<PathBuf> {
    let candidate = if path.components().count() > 1 || path.is_absolute() {
        path.to_path_buf()
    } else {
        let search_path = std::env::var_os("PATH")?;
        std::env::split_paths(&search_path)
            .flat_map(|directory| path_search_candidates(&directory, path))
            .find(|candidate| usable_executable_file(candidate))?
    };
    usable_executable_file(&candidate).then_some(candidate)
}

fn resolve_executable_path(path: &Path) -> Option<PathBuf> {
    std::fs::canonicalize(resolve_executable_launch_path(path)?).ok()
}

fn executable_path_is_available(path: &Path) -> bool {
    resolve_executable_path(path).is_some()
}

/// Resolve bare program names against the parent process PATH before the
/// supervised launch. Preserve the PATH-selected spelling (including a final
/// applet symlink) for argv[0]; the supervisor separately canonicalizes and
/// opens the exact executable inode. This distinction is required for
/// multicall binaries such as coreutils, whose dispatch semantics depend on
/// argv[0]. Explicit absolute/relative paths are preserved so filesystem/spawn
/// failures still occur at the correct supervised stage.
pub(crate) fn resolve_command_launch_path(
    candidate: PathBuf,
    _environment_policy: CommandEnvironmentPolicy,
) -> std::io::Result<PathBuf> {
    if candidate.components().count() == 1 && !candidate.is_absolute() {
        resolve_executable_launch_path(&candidate).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!(
                    "cannot resolve bare tool executable '{}' via PATH",
                    candidate.display()
                ),
            )
        })
    } else {
        Ok(candidate)
    }
}

fn executable_sha256(path: &Path) -> std::io::Result<Sha256Digest> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(Sha256Digest(hasher.finalize().into()))
}

impl RealToolRunner {
    pub(crate) async fn run_supervised_with_stdio(
        &self,
        cmd: ToolCommand,
        binary_path: PathBuf,
        cancel: &CancellationToken,
        stdin_file: Option<Arc<std::fs::File>>,
        stdout_file: Option<Arc<std::fs::File>>,
        stderr_file: Option<Arc<std::fs::File>>,
    ) -> Result<ToolOutput, ToolRunnerError> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
            use crate::convert::script_supervisor::{
                run_supervised, run_supervised_via_item_supervisor, ContainmentPreference,
                RuntimeDirectoryIdentity, ScriptLifecycleEvent, SupervisedCommand,
            };

            let started = Instant::now();
            let launch_path = binary_path;
            let spawn_record = || Self::build_record(&cmd, None, "", "", started.elapsed());
            let reviewed_path = std::fs::canonicalize(&launch_path)
                .map_err(|_| ToolRunnerError::Spawn { command: spawn_record() })?;
            let binary_file = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
                .open(&reviewed_path)
                .map_err(|_| ToolRunnerError::Spawn { command: spawn_record() })?;
            let binary_meta = binary_file.metadata().map_err(ToolRunnerError::Io)?;
            if !binary_meta.is_file() {
                return Err(ToolRunnerError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("tool executable is not a regular file: {}", reviewed_path.display()),
                )));
            }
            let cwd = match cmd.cwd.as_ref() {
                Some(path) => std::fs::canonicalize(path).map_err(ToolRunnerError::Io)?,
                None => std::env::current_dir().map_err(ToolRunnerError::Io)?,
            };
            let cwd_file = std::fs::File::open(&cwd).map_err(ToolRunnerError::Io)?;
            if !cwd_file.metadata().map_err(ToolRunnerError::Io)?.is_dir() {
                return Err(ToolRunnerError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("tool working directory is not a directory: {}", cwd.display()),
                )));
            }

            let runtime_base = std::env::temp_dir().join("tonepoet-execution-supervisor");
            std::fs::create_dir_all(&runtime_base).map_err(ToolRunnerError::Io)?;
            std::fs::set_permissions(&runtime_base, std::fs::Permissions::from_mode(0o700))
                .map_err(ToolRunnerError::Io)?;
            let token = uuid::Uuid::new_v4().simple().to_string();
            let runtime_directory = runtime_base.join(&token);
            std::fs::create_dir(&runtime_directory).map_err(ToolRunnerError::Io)?;
            std::fs::set_permissions(&runtime_directory, std::fs::Permissions::from_mode(0o700))
                .map_err(ToolRunnerError::Io)?;
            let runtime_meta = std::fs::metadata(&runtime_directory).map_err(ToolRunnerError::Io)?;
            let runtime_identity = RuntimeDirectoryIdentity {
                device: runtime_meta.dev(),
                inode: runtime_meta.ino(),
            };

            let mut environment = if cmd.environment_policy == CommandEnvironmentPolicy::InheritAndSet {
                std::env::vars().collect::<BTreeMap<_, _>>()
            } else {
                BTreeMap::new()
            };
            for entry in &cmd.env {
                environment.insert(entry.key.clone(), entry.value.expose().to_string());
            }

            let explicit_execution_item = self.execution_item_id.clone();
            let execution_item = explicit_execution_item.clone()
                .or_else(crate::concurrency::current_execution_item);
            let item_supervisor = if let Some(item_id) = explicit_execution_item.as_deref() {
                Some(self.execution_supervisor.clone().ok_or_else(|| {
                    ToolRunnerError::Io(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        format!("scheduled tool runner for {item_id} was created without an active item-supervisor capability"),
                    ))
                })?)
            } else {
                match execution_item.as_deref() {
                    Some(item_id) => Some(crate::concurrency::runtime_item_supervisor(item_id)
                        .map_err(|error| ToolRunnerError::Io(std::io::Error::new(std::io::ErrorKind::Other, error)))?),
                    None => None,
                }
            };
            let invocation = SupervisedCommand {
                token: token.clone(),
                runtime_directory: runtime_directory.clone(),
                script_file: Arc::new(binary_file),
                working_directory_file: Arc::new(cwd_file),
                // Execute `script_file` by retained FD, but keep the original
                // PATH-selected spelling as argv[0] for multicall dispatch.
                script: launch_path,
                args: cmd.args.clone(),
                working_directory: cwd,
                environment,
                timeout: cmd.timeout,
                runtime_identity,
                containment_preference: ContainmentPreference::Auto,
                helper_executable: None,
                // A queue execution's lifetime descriptors live in its one
                // persistent item supervisor. Fresh per-command helpers are
                // used only for non-queue callers that have no item authority.
                retained_lifetime_files: if item_supervisor.is_some() {
                    Vec::new()
                } else {
                    crate::concurrency::current_supervision_lifetime_files()
                        .map_err(|error| ToolRunnerError::Io(std::io::Error::new(std::io::ErrorKind::Other, error)))?
                },
                stdin_file,
                stdout_file,
                stderr_file,
            };
            let cancellation = cancel.clone();
            let containment_token = invocation.token.clone();
            let containment_runtime = invocation.runtime_directory.clone();
            let event_item = execution_item.clone();
            let supervisor_for_run = item_supervisor.clone();
            let outcome = tokio::task::spawn_blocking(move || {
                let mut lifecycle = |event: &ScriptLifecycleEvent| {
                    if let Some(item_id) = event_item.as_deref() {
                        match event {
                            ScriptLifecycleEvent::ContainmentPrepared { descriptor, .. } => {
                                crate::concurrency::record_execution_containment(
                                    item_id, &containment_token, &containment_runtime, descriptor
                                ).map_err(crate::convert::script_supervisor::ScriptSupervisorError::Internal)?;
                            }
                            ScriptLifecycleEvent::UserCodeReleased { .. } => {
                                crate::concurrency::mark_execution_containment_released(
                                    item_id, &containment_token
                                ).map_err(crate::convert::script_supervisor::ScriptSupervisorError::Internal)?;
                            }
                            _ => {}
                        }
                    }
                    Ok(())
                };
                match supervisor_for_run.as_ref() {
                    Some(supervisor) => run_supervised_via_item_supervisor(
                        &invocation, supervisor, || cancellation.is_cancelled(), &mut lifecycle
                    ),
                    None => run_supervised(&invocation, || cancellation.is_cancelled(), &mut lifecycle),
                }
            }).await.map_err(|error| ToolRunnerError::Io(std::io::Error::new(
                std::io::ErrorKind::Other, format!("tool supervisor task failed: {error}")
            )))?.map_err(|error| ToolRunnerError::Io(std::io::Error::new(
                std::io::ErrorKind::Other, format!("tool supervisor failed: {error}")
            )))?;

            let elapsed = started.elapsed();
            let stdout_tail = String::from_utf8_lossy(&outcome.stdout_tail).into_owned();
            let stderr_tail = String::from_utf8_lossy(&outcome.stderr_tail).into_owned();
            let status = outcome.status;
            let exit = map_exit_status(status);
            let record = Self::build_record(&cmd, Some(exit), &stdout_tail, &stderr_tail, elapsed);
            if outcome.containment_empty {
                if let Some(item_id) = execution_item.as_deref() {
                    let _ = crate::concurrency::clear_execution_containment(item_id, &token);
                }
                let _ = std::fs::remove_dir_all(&runtime_directory);
            }
            if outcome.cancelled {
                return Err(ToolRunnerError::Cancelled { command: record });
            }
            if outcome.timed_out {
                return Err(ToolRunnerError::Timeout { elapsed, command: record });
            }
            if status.success() {
                Ok(ToolOutput { exit, stdout_tail, stderr_tail, elapsed, command: record })
            } else {
                Err(ToolRunnerError::NonZeroExit { exit, stderr_tail, command: record })
            }
        }
        #[cfg(not(unix))]
        {
            // The v24 persistent-lease/containment protocol depends on Unix OFD
            // inheritance. Fail closed rather than silently returning to a
            // third-party-owned execution lifetime.
            let _ = (cmd, binary_path, cancel, stdin_file, stdout_file, stderr_file);
            Err(ToolRunnerError::Io(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "concurrent mutation-capable external tools require tonepoet execution supervision on this platform",
            )))
        }
    }

    pub(crate) async fn run_with_binary_path(
        &self,
        cmd: ToolCommand,
        binary_path: PathBuf,
        cancel: &CancellationToken,
    ) -> Result<ToolOutput, ToolRunnerError> {
        self.run_supervised_with_stdio(cmd, binary_path, cancel, None, None, None).await
    }

}

#[async_trait]
impl ToolRunner for RealToolRunner {
    fn tool_version(&self, binary: ToolBinary) -> Option<String> {
        let path = resolve_command_launch_path(
            self.resolve_binary(binary),
            CommandEnvironmentPolicy::ClearAndSet,
        )
        .ok()?;
        self.tool_version_for_resolved_path(binary, &path)
    }

    fn tool_available(&self, binary: ToolBinary) -> bool {
        executable_path_is_available(&self.resolve_binary(binary))
    }

    fn resolved_tool_path(&self, binary: ToolBinary) -> Option<PathBuf> {
        resolve_executable_path(&self.resolve_binary(binary))
    }

    async fn run(
        &self,
        cmd: ToolCommand,
        cancel: &CancellationToken,
    ) -> Result<ToolOutput, ToolRunnerError> {
        // A closed-environment Reference command must not launch an incidental
        // inherited-environment version probe outside its recorded identity.
        if cmd.environment_policy == CommandEnvironmentPolicy::InheritAndSet {
            let _ = self.tool_version(cmd.binary);
        }

        let binary_path = resolve_command_launch_path(
            self.resolve_binary(cmd.binary),
            cmd.environment_policy,
        )
        .map_err(ToolRunnerError::Io)?;
        self.run_with_binary_path(cmd, binary_path, cancel).await
    }

    async fn run_bound(
        &self,
        cmd: ToolCommand,
        executable: &BoundToolExecutable,
        cancel: &CancellationToken,
    ) -> Result<ToolOutput, ToolRunnerError> {
        let resolved = self.resolved_tool_path(cmd.binary).ok_or_else(|| {
            ToolRunnerError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!(
                    "cannot resolve {} for bound execution",
                    cmd.binary.canonical_name()
                ),
            ))
        })?;
        if resolved != executable.canonical_path {
            return Err(ToolRunnerError::Io(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "bound {} path drift: expected {}, resolved {}",
                    cmd.binary.canonical_name(),
                    executable.canonical_path.display(),
                    resolved.display(),
                ),
            )));
        }
        let actual_sha256 = executable_sha256(&executable.canonical_path)?;
        if actual_sha256 != executable.executable_sha256 {
            return Err(ToolRunnerError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "bound {} executable digest drift at {}: expected {}, got {}",
                    cmd.binary.canonical_name(),
                    executable.canonical_path.display(),
                    executable.executable_sha256,
                    actual_sha256,
                ),
            )));
        }
        self.run_with_binary_path(cmd, executable.canonical_path.clone(), cancel)
            .await
    }


    #[allow(unsafe_code)] // raw fd handling for lease-lifetime file inheritance into spawned tools
    async fn run_pipeline(
        &self,
        producer: ToolCommand,
        consumer: ToolCommand,
        cancel: &CancellationToken,
    ) -> Result<ToolPipelineOutput, ToolPipelineError> {
        #[cfg(unix)]
        {
            use std::os::fd::FromRawFd;
            if producer.environment_policy == CommandEnvironmentPolicy::InheritAndSet {
                let _ = self.tool_version(producer.binary);
            }
            if consumer.environment_policy == CommandEnvironmentPolicy::InheritAndSet {
                let _ = self.tool_version(consumer.binary);
            }
            let producer_path = resolve_command_launch_path(
                self.resolve_binary(producer.binary),
                producer.environment_policy,
            )
            .map_err(|error| ToolPipelineError {
                error: ToolRunnerError::Io(error),
                other_commands: Vec::new(),
            })?;
            let consumer_path = resolve_command_launch_path(
                self.resolve_binary(consumer.binary),
                consumer.environment_policy,
            )
            .map_err(|error| ToolPipelineError {
                error: ToolRunnerError::Io(error),
                other_commands: Vec::new(),
            })?;

            // The transport pipe is ordinary stdio, not ownership authority.
            // Each external endpoint is launched under its own tonepoet
            // supervisor, and both supervisors independently retain the same
            // QueueExecution/path/staging OFDs. A killed originating session
            // therefore cannot orphan mutation-capable producer/consumer work.
            let mut fds = [-1_i32; 2];
            if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
                return Err(ToolPipelineError {
                    error: ToolRunnerError::Io(std::io::Error::last_os_error()),
                    other_commands: Vec::new(),
                });
            }
            // SAFETY: pipe2 returned two newly-owned descriptors.
            let read_end = Arc::new(unsafe { std::fs::File::from_raw_fd(fds[0]) });
            let write_end = Arc::new(unsafe { std::fs::File::from_raw_fd(fds[1]) });

            let pipeline_cancel = CancellationToken::new();
            let producer_future = self.run_supervised_with_stdio(
                producer,
                producer_path,
                &pipeline_cancel,
                None,
                Some(write_end),
                None,
            );
            let consumer_future = self.run_supervised_with_stdio(
                consumer,
                consumer_path,
                &pipeline_cancel,
                Some(read_end),
                None,
                None,
            );
            tokio::pin!(producer_future);
            tokio::pin!(consumer_future);

            fn command_from_error(error: &ToolRunnerError) -> Option<CommandRecord> {
                match error {
                    ToolRunnerError::Spawn { command }
                    | ToolRunnerError::Timeout { command, .. }
                    | ToolRunnerError::Cancelled { command }
                    | ToolRunnerError::Termination { command, .. }
                    | ToolRunnerError::NonZeroExit { command, .. } => Some(command.clone()),
                    ToolRunnerError::UnsupportedPipeline | ToolRunnerError::Io(_) => None,
                }
            }

            fn finish_pipeline(
                producer_result: Result<ToolOutput, ToolRunnerError>,
                consumer_result: Result<ToolOutput, ToolRunnerError>,
                prefer_consumer_error: bool,
            ) -> Result<ToolPipelineOutput, ToolPipelineError> {
                match (producer_result, consumer_result) {
                    (Ok(producer), Ok(consumer)) => Ok(ToolPipelineOutput { producer, consumer }),
                    (Err(error), Ok(consumer)) => Err(ToolPipelineError {
                        error,
                        other_commands: vec![consumer.command],
                    }),
                    (Ok(producer), Err(error)) => Err(ToolPipelineError {
                        error,
                        other_commands: vec![producer.command],
                    }),
                    (Err(producer_error), Err(consumer_error)) => {
                        let consumer_is_primary = prefer_consumer_error
                            || consumer_failure_makes_producer_sigpipe_secondary(
                                &producer_error,
                                &consumer_error,
                            );
                        if consumer_is_primary {
                            let mut other_commands = Vec::new();
                            if let Some(command) = command_from_error(&producer_error) {
                                other_commands.push(command);
                            }
                            Err(ToolPipelineError {
                                error: consumer_error,
                                other_commands,
                            })
                        } else {
                            let mut other_commands = Vec::new();
                            if let Some(command) = command_from_error(&consumer_error) {
                                other_commands.push(command);
                            }
                            Err(ToolPipelineError {
                                error: producer_error,
                                other_commands,
                            })
                        }
                    }
                }
            }

            enum FirstStageResult {
                Producer(Result<ToolOutput, ToolRunnerError>),
                Consumer(Result<ToolOutput, ToolRunnerError>),
                ExternalCancellation,
            }

            let first = tokio::select! {
                biased;
                _ = cancel.cancelled() => FirstStageResult::ExternalCancellation,
                // Poll the producer before the consumer so a consumer-side
                // spawn failure still observes/reaps an actually-started
                // producer, matching ordinary pipeline process ordering.
                result = &mut producer_future => FirstStageResult::Producer(result),
                result = &mut consumer_future => FirstStageResult::Consumer(result),
            };

            enum PeerWait<T> {
                Result(T),
                ExternalCancellation,
            }

            match first {
                FirstStageResult::Producer(producer_result) => {
                    let consumer_result = if producer_result.is_err() {
                        pipeline_cancel.cancel();
                        consumer_future.await
                    } else {
                        let wait = tokio::select! {
                            result = &mut consumer_future => PeerWait::Result(result),
                            _ = cancel.cancelled() => PeerWait::ExternalCancellation,
                        };
                        match wait {
                            PeerWait::Result(result) => result,
                            PeerWait::ExternalCancellation => {
                                pipeline_cancel.cancel();
                                consumer_future.await
                            }
                        }
                    };
                    finish_pipeline(producer_result, consumer_result, false)
                }
                FirstStageResult::Consumer(consumer_result) => {
                    let prefer_consumer_error = consumer_result.is_err();
                    let producer_result = if prefer_consumer_error {
                        // A failed/never-started consumer must close the pipe
                        // and reap the producer immediately rather than
                        // waiting for its independent timeout or pipe backpressure.
                        pipeline_cancel.cancel();
                        producer_future.await
                    } else {
                        let wait = tokio::select! {
                            result = &mut producer_future => PeerWait::Result(result),
                            _ = cancel.cancelled() => PeerWait::ExternalCancellation,
                        };
                        match wait {
                            PeerWait::Result(result) => result,
                            PeerWait::ExternalCancellation => {
                                pipeline_cancel.cancel();
                                producer_future.await
                            }
                        }
                    };
                    finish_pipeline(producer_result, consumer_result, prefer_consumer_error)
                }
                FirstStageResult::ExternalCancellation => {
                    pipeline_cancel.cancel();
                    let (producer_result, consumer_result) =
                        tokio::join!(producer_future, consumer_future);
                    finish_pipeline(producer_result, consumer_result, false)
                }
            }

        }
        #[cfg(not(unix))]
        {
            let _ = (producer, consumer, cancel);
            Err(ToolPipelineError {
                error: ToolRunnerError::Io(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "supervised external pipelines require Unix descriptor handoff",
                )),
                other_commands: Vec::new(),
            })
        }
    }
}

// ===========================================================================
// Chunk 2.1.3 test runner: gated failure/cancellation coordination
// ===========================================================================

#[cfg(test)]
pub(crate) mod blocking_test_runner {
    use super::*;
    use std::collections::VecDeque;
    use std::fs;
    use std::path::PathBuf;
    use tokio::sync::oneshot;

    /// Test-side handle for one blocked tool invocation.
    pub(crate) struct ToolGate {
        started: oneshot::Receiver<()>,
        release: Option<oneshot::Sender<()>>,
    }

    impl ToolGate {
        pub(crate) async fn wait_started(self) -> ToolGateRelease {
            let ToolGate { started, release } = self;
            let _ = started.await;
            ToolGateRelease { release }
        }
    }

    pub(crate) struct ToolGateRelease {
        release: Option<oneshot::Sender<()>>,
    }

    impl ToolGateRelease {
        pub(crate) fn release(mut self) {
            if let Some(release) = self.release.take() {
                let _ = release.send(());
            }
        }
    }

    /// Runner-side blocker for one command. The test owns the paired [`ToolGate`].
    pub(crate) struct ToolBlocker {
        started: Option<oneshot::Sender<()>>,
        release: oneshot::Receiver<()>,
    }

    pub(crate) fn tool_gate() -> (ToolGate, ToolBlocker) {
        let (started_tx, started_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        (
            ToolGate {
                started: started_rx,
                release: Some(release_tx),
            },
            ToolBlocker {
                started: Some(started_tx),
                release: release_rx,
            },
        )
    }

    pub(crate) enum ToolBehavior {
        Succeed,
        SucceedAndWrite { path: PathBuf, bytes: Vec<u8> },
        SucceedAndWriteThenCancel { path: PathBuf, bytes: Vec<u8> },
        FailWithStderr(String),
        FailAfterWriting {
            path: PathBuf,
            bytes: Vec<u8>,
            stderr: String,
        },
        BlockThenSucceed(ToolBlocker),
        BlockThenFail {
            gate: ToolBlocker,
            stderr: String,
        },
        BlockThenSucceedAndWrite {
            gate: ToolBlocker,
            path: PathBuf,
            bytes: Vec<u8>,
        },
    }

    pub(crate) struct BlockingToolRunner {
        behaviors: Mutex<VecDeque<ToolBehavior>>,
        transcript: Mutex<Vec<CommandRecord>>,
    }

    impl BlockingToolRunner {
        pub(crate) fn new() -> Self {
            Self {
                behaviors: Mutex::new(VecDeque::new()),
                transcript: Mutex::new(Vec::new()),
            }
        }

        pub(crate) fn with_behaviors<I>(behaviors: I) -> Self
        where
            I: IntoIterator<Item = ToolBehavior>,
        {
            Self {
                behaviors: Mutex::new(behaviors.into_iter().collect()),
                transcript: Mutex::new(Vec::new()),
            }
        }

        #[allow(dead_code)]
        pub(crate) fn push(&self, behavior: ToolBehavior) {
            self.behaviors.lock().unwrap().push_back(behavior);
        }

        pub(crate) fn transcript(&self) -> Vec<CommandRecord> {
            self.transcript.lock().unwrap().clone()
        }

        fn next_behavior(&self) -> ToolBehavior {
            self.behaviors.lock().unwrap().pop_front().unwrap_or_else(|| {
                panic!(
                    "BlockingToolRunner behavior queue exhausted; enqueue one behavior per expected invocation"
                )
            })
        }

        fn record(cmd: &ToolCommand, exit: Option<ProcessExit>, stderr: &str) -> CommandRecord {
            CommandRecord {
                environment_policy: cmd.environment_policy,
                environment: cmd.sanitized_environment(),
                description: None,
                binary: cmd.binary,
                sanitized_args: cmd.sanitized_args(),
                cwd: cmd.cwd.clone(),
                env_keys: cmd.env_keys(),
                exit,
                stdout_tail: String::new(),
                stderr_tail: stderr.to_string(),
                elapsed: Duration::ZERO,
            }
        }

        fn start_record(&self, cmd: &ToolCommand) -> usize {
            let mut transcript = self.transcript.lock().unwrap();
            let index = transcript.len();
            transcript.push(Self::record(cmd, None, ""));
            index
        }

        fn set_record(&self, index: usize, record: CommandRecord) {
            let mut transcript = self.transcript.lock().unwrap();
            transcript[index] = record;
        }

        fn success_output(&self, index: usize, cmd: &ToolCommand) -> ToolOutput {
            let record = Self::record(cmd, Some(ProcessExit::Code(0)), "");
            self.set_record(index, record.clone());
            ToolOutput {
                exit: ProcessExit::Code(0),
                stdout_tail: String::new(),
                stderr_tail: String::new(),
                elapsed: Duration::ZERO,
                command: record,
            }
        }

        fn failure_record(&self, index: usize, cmd: &ToolCommand, stderr: &str) -> CommandRecord {
            let record = Self::record(cmd, Some(ProcessExit::Code(1)), stderr);
            self.set_record(index, record.clone());
            record
        }

        fn cancelled_record(&self, index: usize, cmd: &ToolCommand) -> CommandRecord {
            let record = Self::record(cmd, None, "cancelled");
            self.set_record(index, record.clone());
            record
        }

        fn write_file(path: PathBuf, bytes: Vec<u8>) -> Result<(), ToolRunnerError> {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(ToolRunnerError::Io)?;
            }
            fs::write(path, bytes).map_err(ToolRunnerError::Io)
        }

        async fn wait_on_gate(
            mut gate: ToolBlocker,
            cancel: &CancellationToken,
        ) -> Result<(), ()> {
            if let Some(started) = gate.started.take() {
                let _ = started.send(());
            }
            tokio::select! {
                _ = cancel.cancelled() => Err(()),
                released = &mut gate.release => match released {
                    Ok(()) => Ok(()),
                    Err(_) if cancel.is_cancelled() => Err(()),
                    Err(_) => panic!(
                        "BlockingToolRunner gate sender dropped without release or cancellation; call ToolGateRelease::release() for success paths"
                    ),
                },
            }
        }
    }

    impl Default for BlockingToolRunner {
        fn default() -> Self {
            Self::new()
        }
    }

    #[async_trait]
    impl ToolRunner for BlockingToolRunner {
        async fn run(
            &self,
            cmd: ToolCommand,
            cancel: &CancellationToken,
        ) -> Result<ToolOutput, ToolRunnerError> {
            let behavior = self.next_behavior();
            let slot = self.start_record(&cmd);

            if cancel.is_cancelled() {
                return Err(ToolRunnerError::Cancelled {
                    command: self.cancelled_record(slot, &cmd),
                });
            }

            match behavior {
                ToolBehavior::Succeed => Ok(self.success_output(slot, &cmd)),
                ToolBehavior::SucceedAndWrite { path, bytes } => {
                    Self::write_file(path, bytes)?;
                    Ok(self.success_output(slot, &cmd))
                }
                ToolBehavior::SucceedAndWriteThenCancel { path, bytes } => {
                    Self::write_file(path, bytes)?;
                    cancel.cancel();
                    Ok(self.success_output(slot, &cmd))
                }
                ToolBehavior::FailWithStderr(stderr_tail) => {
                    let command = self.failure_record(slot, &cmd, &stderr_tail);
                    Err(ToolRunnerError::NonZeroExit {
                        exit: ProcessExit::Code(1),
                        stderr_tail,
                        command,
                    })
                }
                ToolBehavior::FailAfterWriting { path, bytes, stderr } => {
                    Self::write_file(path, bytes)?;
                    let command = self.failure_record(slot, &cmd, &stderr);
                    Err(ToolRunnerError::NonZeroExit {
                        exit: ProcessExit::Code(1),
                        stderr_tail: stderr,
                        command,
                    })
                }
                ToolBehavior::BlockThenSucceed(gate) => {
                    Self::wait_on_gate(gate, cancel)
                        .await
                        .map_err(|()| ToolRunnerError::Cancelled {
                            command: self.cancelled_record(slot, &cmd),
                        })?;
                    Ok(self.success_output(slot, &cmd))
                }
                ToolBehavior::BlockThenFail { gate, stderr } => {
                    Self::wait_on_gate(gate, cancel)
                        .await
                        .map_err(|()| ToolRunnerError::Cancelled {
                            command: self.cancelled_record(slot, &cmd),
                        })?;
                    let command = self.failure_record(slot, &cmd, &stderr);
                    Err(ToolRunnerError::NonZeroExit {
                        exit: ProcessExit::Code(1),
                        stderr_tail: stderr,
                        command,
                    })
                }
                ToolBehavior::BlockThenSucceedAndWrite { gate, path, bytes } => {
                    Self::wait_on_gate(gate, cancel)
                        .await
                        .map_err(|()| ToolRunnerError::Cancelled {
                            command: self.cancelled_record(slot, &cmd),
                        })?;
                    Self::write_file(path, bytes)?;
                    Ok(self.success_output(slot, &cmd))
                }
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::sync::Arc;

        fn cmd(binary: ToolBinary, arg: &str) -> ToolCommand {
            ToolCommand {
                environment_policy: tonepoet_pipeline::CommandEnvironmentPolicy::InheritAndSet,
                binary,
                args: vec![arg.to_string()],
                secret_args: Vec::new(),
                cwd: None,
                env: Vec::new(),
                timeout: Duration::from_secs(60),
            }
        }

        #[tokio::test]
        async fn default_bound_execution_fails_closed() {
            let runner = StubToolRunner::new();
            let authority = BoundToolExecutable {
                canonical_path: PathBuf::from("/qualified/store/bin/metaflac"),
                executable_sha256: Sha256Digest::of_bytes(b"qualified-metaflac"),
            };
            let error = runner
                .run_bound(
                    cmd(ToolBinary::Metaflac, "--version"),
                    &authority,
                    &CancellationToken::new(),
                )
                .await
                .expect_err("an unbound custom runner must fail closed");
            assert!(matches!(
                error,
                ToolRunnerError::Io(ref io)
                    if io.kind() == std::io::ErrorKind::Unsupported
            ));
            assert!(runner.transcript().is_empty());
        }

        #[tokio::test]
        async fn succeeds_fails_and_records_in_order() {
            let runner = BlockingToolRunner::with_behaviors([
                ToolBehavior::Succeed,
                ToolBehavior::FailWithStderr("boom".to_string()),
            ]);
            let cancel = CancellationToken::new();

            runner
                .run(cmd(ToolBinary::Ssrc, "first"), &cancel)
                .await
                .expect("first command succeeds");
            let err = runner
                .run(cmd(ToolBinary::Flac, "second"), &cancel)
                .await
                .expect_err("second command fails");

            assert!(matches!(err, ToolRunnerError::NonZeroExit { .. }));
            let transcript = runner.transcript();
            assert_eq!(transcript.len(), 2);
            assert_eq!(transcript[0].binary, ToolBinary::Ssrc);
            assert_eq!(transcript[0].sanitized_args, vec!["first".to_string()]);
            assert_eq!(transcript[1].binary, ToolBinary::Flac);
            assert_eq!(transcript[1].sanitized_args, vec!["second".to_string()]);
        }

        #[tokio::test]
        async fn blocked_command_returns_cancelled_without_release() {
            let (gate, blocker) = tool_gate();
            let runner = Arc::new(BlockingToolRunner::with_behaviors([
                ToolBehavior::BlockThenSucceed(blocker),
            ]));
            let cancel = CancellationToken::new();
            let run_cancel = cancel.clone();
            let run_runner = runner.clone();
            let handle = tokio::spawn(async move {
                run_runner.run(cmd(ToolBinary::Ssrc, "blocked"), &run_cancel).await
            });

            let release = gate.wait_started().await;
            cancel.cancel();
            let err = handle
                .await
                .expect("tool task joins")
                .expect_err("blocked command observes cancellation");

            assert!(matches!(err, ToolRunnerError::Cancelled { .. }));
            let transcript = runner.transcript();
            assert_eq!(transcript.len(), 1);
            assert_eq!(transcript[0].exit, None);
            assert_eq!(transcript[0].stderr_tail, "cancelled");
            drop(release);
        }

        #[tokio::test]
        #[should_panic(expected = "behavior queue exhausted")]
        async fn behavior_exhaustion_fails_loudly() {
            let runner = BlockingToolRunner::new();
            let cancel = CancellationToken::new();
            let _ = runner.run(cmd(ToolBinary::Ssrc, "unexpected"), &cancel).await;
        }


        #[tokio::test]
        async fn dropped_release_sender_without_cancel_is_test_harness_error() {
            let (gate, blocker) = tool_gate();
            let runner = Arc::new(BlockingToolRunner::with_behaviors([
                ToolBehavior::BlockThenSucceed(blocker),
            ]));
            let cancel = CancellationToken::new();
            let run_cancel = cancel.clone();
            let run_runner = runner.clone();
            let handle = tokio::spawn(async move {
                run_runner.run(cmd(ToolBinary::Ssrc, "blocked"), &run_cancel).await
            });

            let release = gate.wait_started().await;
            drop(release);
            let join_err = handle.await.expect_err("dropped release sender panics");
            assert!(join_err.is_panic());
        }

        #[tokio::test]
        async fn concurrent_records_update_their_own_slots() {
            let (first_gate, first_blocker) = tool_gate();
            let (second_gate, second_blocker) = tool_gate();
            let runner = Arc::new(BlockingToolRunner::with_behaviors([
                ToolBehavior::BlockThenSucceed(first_blocker),
                ToolBehavior::BlockThenFail {
                    gate: second_blocker,
                    stderr: "second failed".to_string(),
                },
            ]));
            let cancel = CancellationToken::new();

            let first_runner = runner.clone();
            let first_cancel = cancel.clone();
            let first = tokio::spawn(async move {
                first_runner.run(cmd(ToolBinary::Ffmpeg, "first"), &first_cancel).await
            });
            let first_release = first_gate.wait_started().await;

            let second_runner = runner.clone();
            let second_cancel = cancel.clone();
            let second = tokio::spawn(async move {
                second_runner.run(cmd(ToolBinary::Sox, "second"), &second_cancel).await
            });
            let second_release = second_gate.wait_started().await;

            first_release.release();
            second_release.release();
            first.await.expect("first joins").expect("first succeeds");
            assert!(matches!(
                second.await.expect("second joins"),
                Err(ToolRunnerError::NonZeroExit { .. })
            ));

            let transcript = runner.transcript();
            assert_eq!(transcript.len(), 2);
            assert_eq!(transcript[0].binary, ToolBinary::Ffmpeg);
            assert_eq!(transcript[0].sanitized_args, vec!["first".to_string()]);
            assert_eq!(transcript[0].exit, Some(ProcessExit::Code(0)));
            assert_eq!(transcript[1].binary, ToolBinary::Sox);
            assert_eq!(transcript[1].sanitized_args, vec!["second".to_string()]);
            assert_eq!(transcript[1].exit, Some(ProcessExit::Code(1)));
            assert_eq!(transcript[1].stderr_tail, "second failed");
        }
    }
}

#[cfg(test)]
mod real_tool_runner_tests {
    use super::*;
    use std::collections::HashMap;
    use std::time::Duration;

    fn runner_with_override(binary: ToolBinary, program: &str) -> RealToolRunner {
        let mut paths = HashMap::new();
        paths.insert(binary.canonical_name().to_string(), PathBuf::from(program));
        RealToolRunner::new(paths)
    }

    #[cfg(unix)]
    #[test]
    fn inherited_environment_bare_program_resolves_from_path_before_supervision() {
        let bare = PathBuf::from("sh");
        let expected_launch =
            resolve_executable_launch_path(&bare).expect("test PATH should provide sh");
        let expected_identity =
            resolve_executable_path(&bare).expect("test PATH should canonicalize sh");
        let resolved = resolve_command_launch_path(
            bare,
            tonepoet_pipeline::CommandEnvironmentPolicy::InheritAndSet,
        )
        .expect("bare sh should resolve through PATH");
        assert_eq!(resolved, expected_launch);
        assert_eq!(std::fs::canonicalize(&resolved).unwrap(), expected_identity);
    }

    #[test]
    fn bare_program_missing_from_path_is_not_reinterpreted_relative_to_cwd() {
        let bare = PathBuf::from(
            "tonepoet-path-miss-regression-9f3c65b7-0ec7-4f16-9f28-b7e77f695a4a",
        );
        assert!(
            resolve_executable_path(&bare).is_none(),
            "test precondition: unique bare name must be absent from PATH"
        );

        let error = resolve_command_launch_path(
            bare,
            tonepoet_pipeline::CommandEnvironmentPolicy::InheritAndSet,
        )
        .expect_err("a bare PATH miss must fail before supervised canonicalization");
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    }

    #[tokio::test]
    async fn normal_run_propagates_bare_path_miss_as_not_found() {
        let program = "tonepoet-path-miss-run-regression-4506a7e8-7d13-4581-b1ee-812b05b3b434";
        let bare = PathBuf::from(program);
        assert!(
            resolve_executable_path(&bare).is_none(),
            "test precondition: unique bare name must be absent from PATH"
        );

        let runner = runner_with_override(ToolBinary::Ffmpeg, program);
        let error = runner
            .run(
                ToolCommand {
                    environment_policy:
                        tonepoet_pipeline::CommandEnvironmentPolicy::InheritAndSet,
                    binary: ToolBinary::Ffmpeg,
                    args: Vec::new(),
                    secret_args: Vec::new(),
                    cwd: None,
                    env: Vec::new(),
                    timeout: Duration::from_secs(1),
                },
                &CancellationToken::new(),
            )
            .await
            .expect_err("a bare PATH miss must fail before supervised canonicalization");
        assert!(matches!(
            error,
            ToolRunnerError::Io(ref io) if io.kind() == std::io::ErrorKind::NotFound
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pipeline_propagates_bare_path_miss_as_not_found() {
        let missing =
            "tonepoet-path-miss-pipeline-regression-3ec73960-528e-418a-9e0c-7694c951cf32";
        let mut paths = HashMap::new();
        paths.insert(ToolBinary::Ffmpeg.canonical_name().to_string(), PathBuf::from(missing));
        paths.insert(ToolBinary::Sox.canonical_name().to_string(), PathBuf::from("sh"));
        let runner = RealToolRunner::new(paths);

        let command = |binary| ToolCommand {
            environment_policy: tonepoet_pipeline::CommandEnvironmentPolicy::InheritAndSet,
            binary,
            args: Vec::new(),
            secret_args: Vec::new(),
            cwd: None,
            env: Vec::new(),
            timeout: Duration::from_secs(1),
        };
        let error = runner
            .run_pipeline(
                command(ToolBinary::Ffmpeg),
                command(ToolBinary::Sox),
                &CancellationToken::new(),
            )
            .await
            .expect_err("pipeline must fail before launching when a bare tool is absent from PATH");
        assert!(matches!(
            error.error,
            ToolRunnerError::Io(ref io) if io.kind() == std::io::ErrorKind::NotFound
        ));
        assert!(error.other_commands.is_empty());
    }

    #[test]
    fn explicit_relative_program_path_is_preserved_for_filesystem_resolution() {
        let relative = PathBuf::from(
            "./tonepoet-relative-tool-regression-72821aae-e206-46ee-96d1-135fe9fce270",
        );
        assert!(relative.components().count() > 1);
        let resolved = resolve_command_launch_path(
            relative.clone(),
            tonepoet_pipeline::CommandEnvironmentPolicy::InheritAndSet,
        )
        .expect("explicit relative path is not a PATH lookup");
        assert_eq!(resolved, relative);

        let error = std::fs::canonicalize(&resolved)
            .expect_err("unique explicit relative fixture should be absent from the filesystem");
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    }

    #[tokio::test]
    async fn timeout_kills_hung_process() {
        let runner = runner_with_override(ToolBinary::Ffmpeg, "sleep");
        let cancel = CancellationToken::new();
        let cmd = ToolCommand {
            environment_policy: tonepoet_pipeline::CommandEnvironmentPolicy::InheritAndSet,
            binary: ToolBinary::Ffmpeg,
            args: vec!["999".to_string()],
            secret_args: vec![],
            cwd: None,
            env: vec![],
            timeout: Duration::from_millis(100),
        };

        let start = std::time::Instant::now();
        let result = runner.run(cmd, &cancel).await;
        let elapsed = start.elapsed();

        assert!(
            matches!(result, Err(ToolRunnerError::Timeout { .. })),
            "expected Timeout, got {result:?}"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "timeout should kill quickly, took {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn cancellation_kills_running_process() {
        let runner = runner_with_override(ToolBinary::Ffmpeg, "sleep");
        let cancel = CancellationToken::new();
        let cmd = ToolCommand {
            environment_policy: tonepoet_pipeline::CommandEnvironmentPolicy::InheritAndSet,
            binary: ToolBinary::Ffmpeg,
            args: vec!["999".to_string()],
            secret_args: vec![],
            cwd: None,
            env: vec![],
            timeout: Duration::from_secs(60),
        };

        let cancel_clone = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            cancel_clone.cancel();
        });

        let start = std::time::Instant::now();
        let result = runner.run(cmd, &cancel).await;
        let elapsed = start.elapsed();

        assert!(
            matches!(result, Err(ToolRunnerError::Cancelled { .. })),
            "expected Cancelled, got {result:?}"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "cancellation should kill quickly, took {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn normal_completion_within_timeout() {
        let runner = runner_with_override(ToolBinary::Ffmpeg, "echo");
        let cancel = CancellationToken::new();
        let cmd = ToolCommand {
            environment_policy: tonepoet_pipeline::CommandEnvironmentPolicy::InheritAndSet,
            binary: ToolBinary::Ffmpeg,
            args: vec!["hello".to_string()],
            secret_args: vec![],
            cwd: None,
            env: vec![],
            timeout: Duration::from_secs(5),
        };

        let result = runner.run(cmd, &cancel).await;
        let output = result.expect("echo should succeed");

        assert_eq!(output.exit, ProcessExit::Code(0));
        assert!(
            output.stdout_tail.contains("hello"),
            "stdout should contain 'hello', got: {}",
            output.stdout_tail
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn real_pipeline_connects_producer_stdout_directly_to_consumer_stdin() {
        let producer_script = write_executable_script(
            "fake-sox-pipeline",
            r#"#!/bin/sh
if [ "$1" = "--help" ]; then
  printf 'SoX_ng v14.8.0.1\n'
  exit 0
fi
printf 'exact-pipeline-payload'
printf 'producer diagnostic\n' >&2
"#,
        );
        let consumer_script = write_executable_script(
            "fake-ffmpeg-pipeline",
            r#"#!/bin/sh
if [ "$1" = "--version" ]; then
  printf 'ffmpeg version 7.1.0\n'
  exit 0
fi
cat
printf 'consumer diagnostic\n' >&2
"#,
        );
        let mut paths = HashMap::new();
        paths.insert(
            ToolBinary::Sox.canonical_name().to_string(),
            producer_script,
        );
        paths.insert(
            ToolBinary::Ffmpeg.canonical_name().to_string(),
            consumer_script,
        );
        let runner = RealToolRunner::new(paths);
        let command = |binary| ToolCommand {
            environment_policy: tonepoet_pipeline::CommandEnvironmentPolicy::InheritAndSet,
            binary,
            args: Vec::new(),
            secret_args: Vec::new(),
            cwd: None,
            env: Vec::new(),
            timeout: Duration::from_secs(5),
        };

        let output = runner
            .run_pipeline(
                command(ToolBinary::Sox),
                command(ToolBinary::Ffmpeg),
                &CancellationToken::new(),
            )
            .await
            .expect("typed pipeline succeeds");

        assert_eq!(output.producer.exit, ProcessExit::Code(0));
        assert_eq!(output.consumer.exit, ProcessExit::Code(0));
        assert!(output.producer.stdout_tail.is_empty());
        assert_eq!(output.consumer.stdout_tail, "exact-pipeline-payload");
        assert!(output.producer.stderr_tail.contains("producer diagnostic"));
        assert!(output.consumer.stderr_tail.contains("consumer diagnostic"));
    }

    fn closed_command(
        binary: ToolBinary,
        args: Vec<String>,
        timeout: Duration,
    ) -> ToolCommand {
        ToolCommand {
            binary,
            args,
            secret_args: Vec::new(),
            cwd: None,
            environment_policy: tonepoet_pipeline::CommandEnvironmentPolicy::ClearAndSet,
            env: vec![EnvVar {
                key: "LC_ALL".to_string(),
                value: SecretString::new("C"),
                secret: false,
            }],
            timeout,
        }
    }

    fn pipeline_error_records(error: &ToolPipelineError) -> Vec<&CommandRecord> {
        let mut records = error.other_commands.iter().collect::<Vec<_>>();
        match &error.error {
            ToolRunnerError::Spawn { command }
            | ToolRunnerError::Timeout { command, .. }
            | ToolRunnerError::Cancelled { command }
            | ToolRunnerError::Termination { command, .. }
            | ToolRunnerError::NonZeroExit { command, .. } => records.push(command),
            ToolRunnerError::UnsupportedPipeline | ToolRunnerError::Io(_) => {}
        }
        records
    }

    #[cfg(target_os = "linux")]
    fn assert_pid_reaped(path: &std::path::Path) {
        let pid: u32 = std::fs::read_to_string(path)
            .unwrap_or_else(|error| panic!("read child pid {}: {error}", path.display()))
            .trim()
            .parse()
            .expect("child pid parses");
        let proc_path = PathBuf::from(format!("/proc/{pid}"));
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while proc_path.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !proc_path.exists(),
            "child process {pid} still exists after runner returned"
        );
    }

    #[tokio::test]
    async fn default_runner_rejects_pipeline_instead_of_simulating_one() {
        let runner = StubToolRunner::new();
        let command = closed_command(ToolBinary::Sox, Vec::new(), Duration::from_secs(1));
        let error = runner
            .run_pipeline(command.clone(), command, &CancellationToken::new())
            .await
            .expect_err("default pipeline implementation must be unsupported");
        assert!(matches!(&error.error, ToolRunnerError::UnsupportedPipeline));
        assert!(error.other_commands.is_empty());
        assert!(runner.transcript().is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn clear_and_set_command_excludes_ambient_environment_and_records_identity() {
        let script = write_executable_script(
            "closed-env-command",
            r#"#!/bin/sh
printf 'home=%s path=%s lc_all=%s\n' "${HOME-unset}" "${PATH-unset}" "${LC_ALL-unset}"
"#,
        );
        let runner = runner_with_override(ToolBinary::Ffmpeg, script.to_str().unwrap());
        let output = runner
            .run(
                closed_command(ToolBinary::Ffmpeg, Vec::new(), Duration::from_secs(2)),
                &CancellationToken::new(),
            )
            .await
            .expect("closed-environment command succeeds");
        // A shell exec'd with a cleared environment self-assigns the libc
        // default PATH (_PATH_DEFPATH), so PATH can never probe as literally
        // unset. Clearing is proven by HOME being unset and the ambient PATH
        // being absent; the allowlist by LC_ALL=C.
        let tail = output.stdout_tail.trim().to_string();
        assert!(tail.starts_with("home=unset path="), "{tail}");
        assert!(tail.ends_with("lc_all=C"), "{tail}");
        assert!(
            !tail.contains(&std::env::var("PATH").unwrap_or_default()),
            "ambient PATH leaked into the cleared child: {tail}"
        );
        assert_eq!(
            output.command.environment_policy,
            tonepoet_pipeline::CommandEnvironmentPolicy::ClearAndSet
        );
        assert_eq!(
            output.command.environment,
            BTreeMap::from([("LC_ALL".to_string(), "C".to_string())])
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn clear_and_set_pipeline_excludes_ambient_environment_in_both_stages() {
        let producer_script = write_executable_script(
            "closed-env-producer",
            r#"#!/bin/sh
printf 'producer-home=%s producer-path=%s producer-lc=%s\n' "${HOME-unset}" "${PATH-unset}" "${LC_ALL-unset}"
"#,
        );
        let consumer_script = write_executable_script(
            "closed-env-consumer",
            r#"#!/bin/sh
IFS= read -r payload
printf '%s consumer-home=%s consumer-path=%s consumer-lc=%s\n' "$payload" "${HOME-unset}" "${PATH-unset}" "${LC_ALL-unset}"
"#,
        );
        let mut paths = HashMap::new();
        paths.insert(ToolBinary::Sox.canonical_name().to_string(), producer_script);
        paths.insert(
            ToolBinary::Ffmpeg.canonical_name().to_string(),
            consumer_script,
        );
        let output = RealToolRunner::new(paths)
            .run_pipeline(
                closed_command(ToolBinary::Sox, Vec::new(), Duration::from_secs(2)),
                closed_command(ToolBinary::Ffmpeg, Vec::new(), Duration::from_secs(2)),
                &CancellationToken::new(),
            )
            .await
            .expect("closed-environment pipeline succeeds");
        // See the single-command probe: a cleared shell self-assigns the
        // libc default PATH, so both stages assert HOME/LC_ALL plus absence
        // of the ambient PATH instead of a literally unset PATH.
        let tail = output.consumer.stdout_tail.trim().to_string();
        assert!(tail.starts_with("producer-home=unset producer-path="), "{tail}");
        assert!(tail.contains("producer-lc=C consumer-home=unset consumer-path="), "{tail}");
        assert!(tail.ends_with("consumer-lc=C"), "{tail}");
        assert!(
            !tail.contains(&std::env::var("PATH").unwrap_or_default()),
            "ambient PATH leaked into a cleared pipeline stage: {tail}"
        );
        for record in [&output.producer.command, &output.consumer.command] {
            assert_eq!(
                record.environment_policy,
                tonepoet_pipeline::CommandEnvironmentPolicy::ClearAndSet
            );
            assert_eq!(
                record.environment,
                BTreeMap::from([("LC_ALL".to_string(), "C".to_string())])
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pipeline_producer_timeout_terminates_and_reaps_both_stages() {
        let pid_path = std::env::temp_dir().join(format!(
            "tonepoet-producer-timeout-pid-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let producer = write_executable_script(
            "timeout-producer",
            r#"#!/bin/sh
printf '%s\n' "$$" > "$1"
exec /bin/sleep 30
"#,
        );
        let consumer = write_executable_script(
            "timeout-consumer",
            r#"#!/bin/sh
exec /bin/cat >/dev/null
"#,
        );
        let mut paths = HashMap::new();
        paths.insert(ToolBinary::Sox.canonical_name().to_string(), producer);
        paths.insert(ToolBinary::Ffmpeg.canonical_name().to_string(), consumer);
        let error = RealToolRunner::new(paths)
            .run_pipeline(
                closed_command(
                    ToolBinary::Sox,
                    vec![pid_path.display().to_string()],
                    Duration::from_millis(500),
                ),
                closed_command(ToolBinary::Ffmpeg, Vec::new(), Duration::from_secs(5)),
                &CancellationToken::new(),
            )
            .await
            .expect_err("producer timeout fails pipeline");
        assert!(matches!(&error.error, ToolRunnerError::Timeout { .. }));
        assert!(
            pipeline_error_records(&error)
                .iter()
                .all(|record| record.exit.is_some()),
            "timeout returned without terminal statuses: {error:?}"
        );
        #[cfg(target_os = "linux")]
        assert_pid_reaped(&pid_path);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pipeline_consumer_timeout_terminates_and_reaps_both_stages() {
        let pid_path = std::env::temp_dir().join(format!(
            "tonepoet-consumer-timeout-pid-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let producer = write_executable_script(
            "finite-producer",
            "#!/bin/sh\nprintf 'payload'\n",
        );
        let consumer = write_executable_script(
            "timeout-consumer",
            r#"#!/bin/sh
printf '%s\n' "$$" > "$1"
exec /bin/sleep 30
"#,
        );
        let mut paths = HashMap::new();
        paths.insert(ToolBinary::Sox.canonical_name().to_string(), producer);
        paths.insert(ToolBinary::Ffmpeg.canonical_name().to_string(), consumer);
        let error = RealToolRunner::new(paths)
            .run_pipeline(
                closed_command(ToolBinary::Sox, Vec::new(), Duration::from_secs(5)),
                closed_command(
                    ToolBinary::Ffmpeg,
                    vec![pid_path.display().to_string()],
                    Duration::from_millis(500),
                ),
                &CancellationToken::new(),
            )
            .await
            .expect_err("consumer timeout fails pipeline");
        assert!(matches!(&error.error, ToolRunnerError::Timeout { .. }));
        assert!(
            pipeline_error_records(&error)
                .iter()
                .all(|record| record.exit.is_some())
        );
        #[cfg(target_os = "linux")]
        assert_pid_reaped(&pid_path);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pipeline_cancellation_terminates_and_reaps_both_stages() {
        let unique = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let producer_pid_path = std::env::temp_dir()
            .join(format!("tonepoet-pipeline-cancel-producer-pid-{unique}"));
        let consumer_pid_path = std::env::temp_dir()
            .join(format!("tonepoet-pipeline-cancel-consumer-pid-{unique}"));
        let producer = write_executable_script(
            "cancel-producer",
            r#"#!/bin/sh
printf '%s\n' "$$" > "$1"
exec /bin/sleep 30
"#,
        );
        let consumer = write_executable_script(
            "cancel-consumer",
            r#"#!/bin/sh
printf '%s\n' "$$" > "$1"
exec /bin/cat >/dev/null
"#,
        );
        let mut paths = HashMap::new();
        paths.insert(ToolBinary::Sox.canonical_name().to_string(), producer);
        paths.insert(ToolBinary::Ffmpeg.canonical_name().to_string(), consumer);
        let cancel = CancellationToken::new();
        let trigger = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            trigger.cancel();
        });
        let error = RealToolRunner::new(paths)
            .run_pipeline(
                closed_command(
                    ToolBinary::Sox,
                    vec![producer_pid_path.display().to_string()],
                    Duration::from_secs(5),
                ),
                closed_command(
                    ToolBinary::Ffmpeg,
                    vec![consumer_pid_path.display().to_string()],
                    Duration::from_secs(5),
                ),
                &cancel,
            )
            .await
            .expect_err("cancelled pipeline fails closed");
        assert!(matches!(&error.error, ToolRunnerError::Cancelled { .. }));
        assert!(
            pipeline_error_records(&error)
                .iter()
                .all(|record| record.exit.is_some())
        );
        #[cfg(target_os = "linux")]
        {
            assert_pid_reaped(&producer_pid_path);
            assert_pid_reaped(&consumer_pid_path);
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pipeline_reports_producer_nonzero_after_reaping_consumer() {
        let producer = write_executable_script("failing-producer", "#!/bin/sh\nexit 7\n");
        let consumer = write_executable_script(
            "draining-consumer",
            "#!/bin/sh\nexec /bin/cat >/dev/null\n",
        );
        let mut paths = HashMap::new();
        paths.insert(ToolBinary::Sox.canonical_name().to_string(), producer);
        paths.insert(ToolBinary::Ffmpeg.canonical_name().to_string(), consumer);
        let error = RealToolRunner::new(paths)
            .run_pipeline(
                closed_command(ToolBinary::Sox, Vec::new(), Duration::from_secs(2)),
                closed_command(ToolBinary::Ffmpeg, Vec::new(), Duration::from_secs(2)),
                &CancellationToken::new(),
            )
            .await
            .expect_err("producer failure fails pipeline");
        assert!(matches!(
            &error.error,
            ToolRunnerError::NonZeroExit {
                exit: ProcessExit::Code(7),
                ..
            }
        ));
        assert!(pipeline_error_records(&error).iter().all(|record| record.exit.is_some()));
    }

    #[cfg(unix)]
    #[test]
    fn producer_sigpipe_is_secondary_only_to_substantive_consumer_failure() {
        fn record(binary: ToolBinary, exit: ProcessExit) -> CommandRecord {
            let command = closed_command(binary, Vec::new(), Duration::from_secs(2));
            RealToolRunner::build_record(
                &command,
                Some(exit),
                "",
                "",
                Duration::from_secs(0),
            )
        }

        let producer_sigpipe = ToolRunnerError::NonZeroExit {
            exit: ProcessExit::Signal(libc::SIGPIPE),
            stderr_tail: String::new(),
            command: record(ToolBinary::Sox, ProcessExit::Signal(libc::SIGPIPE)),
        };
        let consumer_nonzero = ToolRunnerError::NonZeroExit {
            exit: ProcessExit::Code(9),
            stderr_tail: String::new(),
            command: record(ToolBinary::Ffmpeg, ProcessExit::Code(9)),
        };
        assert!(consumer_failure_makes_producer_sigpipe_secondary(
            &producer_sigpipe,
            &consumer_nonzero,
        ));

        let consumer_cancelled = ToolRunnerError::Cancelled {
            command: record(ToolBinary::Ffmpeg, ProcessExit::Signal(libc::SIGTERM)),
        };
        assert!(
            !consumer_failure_makes_producer_sigpipe_secondary(
                &producer_sigpipe,
                &consumer_cancelled,
            ),
            "peer cancellation must not replace a producer failure",
        );

        let producer_nonzero = ToolRunnerError::NonZeroExit {
            exit: ProcessExit::Code(7),
            stderr_tail: String::new(),
            command: record(ToolBinary::Sox, ProcessExit::Code(7)),
        };
        assert!(
            !consumer_failure_makes_producer_sigpipe_secondary(
                &producer_nonzero,
                &consumer_nonzero,
            ),
            "ordinary producer failures must remain primary when observed first",
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pipeline_reports_consumer_nonzero_and_closes_producer() {
        let producer = write_executable_script(
            "infinite-producer",
            "#!/bin/sh\nwhile :; do printf x; done\n",
        );
        let consumer = write_executable_script("failing-consumer", "#!/bin/sh\nexit 9\n");
        let mut paths = HashMap::new();
        paths.insert(ToolBinary::Sox.canonical_name().to_string(), producer);
        paths.insert(ToolBinary::Ffmpeg.canonical_name().to_string(), consumer);
        let error = RealToolRunner::new(paths)
            .run_pipeline(
                closed_command(ToolBinary::Sox, Vec::new(), Duration::from_secs(2)),
                closed_command(ToolBinary::Ffmpeg, Vec::new(), Duration::from_secs(2)),
                &CancellationToken::new(),
            )
            .await
            .expect_err("consumer failure fails pipeline");
        assert!(matches!(
            &error.error,
            ToolRunnerError::NonZeroExit {
                exit: ProcessExit::Code(9),
                ..
            }
        ));
        assert!(pipeline_error_records(&error).iter().all(|record| record.exit.is_some()));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn consumer_spawn_failure_reaps_started_producer() {
        let producer = write_executable_script(
            "spawn-failure-producer",
            "#!/bin/sh\nexec /bin/sleep 30\n",
        );
        let mut paths = HashMap::new();
        paths.insert(ToolBinary::Sox.canonical_name().to_string(), producer);
        paths.insert(
            ToolBinary::Ffmpeg.canonical_name().to_string(),
            PathBuf::from("/definitely/not/a/tonepoet/consumer"),
        );
        let error = RealToolRunner::new(paths)
            .run_pipeline(
                closed_command(ToolBinary::Sox, Vec::new(), Duration::from_secs(2)),
                closed_command(ToolBinary::Ffmpeg, Vec::new(), Duration::from_secs(2)),
                &CancellationToken::new(),
            )
            .await
            .expect_err("consumer spawn failure fails pipeline");
        assert!(matches!(&error.error, ToolRunnerError::Spawn { .. }));
        assert_eq!(error.other_commands.len(), 1);
        assert!(error.other_commands[0].exit.is_some());
    }

    #[test]
    fn parses_supported_tool_version_formats() {
        let cases = [
            (ToolBinary::Ffmpeg, "ffmpeg version 7.1.3 Copyright...", "", "7.1.3"),
            (ToolBinary::Ffprobe, "ffprobe version 7.1.3 Copyright...", "", "7.1.3"),
            (ToolBinary::Sox, "sox:      SoX_ng v14.6.1", "", "14.6.1"),
            (ToolBinary::Ssrc, "", "Shibatch Sample Rate Converter  Version 2.4.2", "2.4.2"),
            (ToolBinary::SevenZip, "7-Zip 25.01 (x64) : Copyright...", "", "25.01"),
            (ToolBinary::Metaflac, "metaflac 1.5.0", "", "1.5.0"),
            (ToolBinary::Flac, "flac 1.5.0", "", "1.5.0"),
            (ToolBinary::Loudgain, "loudgain 0.6.8 - using:", "", "0.6.8"),
            (ToolBinary::Opustags, "opustags version 1.10.1", "", "1.10.1"),
            (ToolBinary::Wvtag, "wvtag 5.8.1", "", "5.8.1"),
            (ToolBinary::Wvunpack, "wvunpack 5.8.1", "", "5.8.1"),
            (
                ToolBinary::AtomicParsley,
                "AtomicParsley version: 20240608.083822.1ed9031",
                "",
                "20240608.083822.1ed9031",
            ),
        ];

        for (binary, stdout, stderr, expected) in cases {
            assert_eq!(
                parse_tool_version_output(binary, stdout, stderr),
                Some(expected.to_string()),
                "unexpected parse result for {binary:?}"
            );
        }
        assert_eq!(
            parse_tool_version_output(ToolBinary::Ffmpeg, "ffmpeg build info without a version", ""),
            None
        );
    }

    #[test]
    fn atomic_parsley_version_probe_uses_zero_argument_banner() {
        assert!(version_command_args(ToolBinary::AtomicParsley).is_empty());
    }

    #[test]
    fn stub_tool_runner_default_version_is_none() {
        let runner = StubToolRunner::new();
        assert_eq!(runner.tool_version(ToolBinary::Ffmpeg), None);
    }

    #[cfg(unix)]
    fn write_executable_script(name: &str, body: &str) -> PathBuf {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let unique = format!(
            "tonepoet-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let dir = std::env::temp_dir().join(unique);
        fs::create_dir_all(&dir).expect("script temp dir");
        let path = dir.join(name);
        // Inject a side-effect-free selfcheck arm right after the shebang so
        // the ETXTBSY verify loop below can execute the script without
        // touching any fixture state (several bodies mutate count files
        // unconditionally).
        let (shebang, rest) = body
            .split_once('\n')
            .expect("fixture scripts start with a shebang line");
        let guarded = format!(
            "{shebang}\nif [ \"$1\" = \"--tonepoet-fixture-selfcheck\" ]; then exit 0; fi\n{rest}"
        );
        fs::write(&path, guarded).expect("write script");
        let mut perms = fs::metadata(&path).expect("script metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).expect("chmod script");
        // Write-then-exec is racy in a threaded test process: a concurrent
        // test's fork can inherit this file's write fd for a moment, and a
        // direct exec then fails with ETXTBSY ("Text file busy"). Verify the
        // script is executable before handing it out; one successful exec
        // proves no writer fd survives, and nothing rewrites the file after.
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            match std::process::Command::new(&path)
                .arg("--tonepoet-fixture-selfcheck")
                .output()
            {
                Ok(output) if output.status.success() => break,
                Ok(output) => panic!(
                    "fixture script selfcheck failed for {}: {:?}",
                    path.display(),
                    output.status
                ),
                // ETXTBSY (errno 26): a racing fork still holds the write fd.
                Err(err) if err.raw_os_error() == Some(26) && std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(err) => panic!(
                    "fixture script selfcheck could not execute {}: {err}",
                    path.display()
                ),
            }
        }
        path
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bound_execution_spawns_the_exact_attested_executable() {
        let script = write_executable_script(
            "bound-exact",
            "#!/bin/sh
printf 'bound-exact\n'
",
        );
        let canonical = std::fs::canonicalize(&script).expect("canonical bound script");
        let authority = BoundToolExecutable {
            canonical_path: canonical.clone(),
            executable_sha256: executable_sha256(&canonical).expect("hash bound script"),
        };
        let runner = runner_with_override(ToolBinary::Metaflac, canonical.to_str().unwrap());
        let output = runner
            .run_bound(
                ToolCommand {
                    environment_policy: CommandEnvironmentPolicy::ClearAndSet,
                    binary: ToolBinary::Metaflac,
                    args: Vec::new(),
                    secret_args: Vec::new(),
                    cwd: None,
                    env: vec![EnvVar {
                        key: "LC_ALL".to_string(),
                        value: SecretString::new("C"),
                        secret: false,
                    }],
                    timeout: Duration::from_secs(5),
                },
                &authority,
                &CancellationToken::new(),
            )
            .await
            .expect("bound execution succeeds");
        assert_eq!(output.exit, ProcessExit::Code(0));
        assert!(output.stdout_tail.contains("bound-exact"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bound_execution_rejects_runner_path_override_drift() {
        let certified = write_executable_script("bound-certified", "#!/bin/sh
exit 0
");
        let replacement = write_executable_script("bound-replacement", "#!/bin/sh
exit 0
");
        let certified = std::fs::canonicalize(certified).expect("canonical certified script");
        let replacement = std::fs::canonicalize(replacement).expect("canonical replacement script");
        let authority = BoundToolExecutable {
            canonical_path: certified.clone(),
            executable_sha256: executable_sha256(&certified).expect("hash certified script"),
        };
        let runner = runner_with_override(ToolBinary::Wvtag, replacement.to_str().unwrap());
        let error = runner
            .run_bound(
                ToolCommand {
                    environment_policy: CommandEnvironmentPolicy::ClearAndSet,
                    binary: ToolBinary::Wvtag,
                    args: Vec::new(),
                    secret_args: Vec::new(),
                    cwd: None,
                    env: Vec::new(),
                    timeout: Duration::from_secs(5),
                },
                &authority,
                &CancellationToken::new(),
            )
            .await
            .expect_err("configured replacement must be rejected");
        assert!(matches!(error, ToolRunnerError::Io(ref io) if io.kind() == std::io::ErrorKind::PermissionDenied));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bound_execution_rejects_executable_content_drift() {
        let script = write_executable_script("bound-digest", "#!/bin/sh
exit 0
");
        let canonical = std::fs::canonicalize(script).expect("canonical bound script");
        let authority = BoundToolExecutable {
            canonical_path: canonical.clone(),
            executable_sha256: Sha256Digest::of_bytes(b"not-the-executable"),
        };
        let runner = runner_with_override(ToolBinary::AtomicParsley, canonical.to_str().unwrap());
        let error = runner
            .run_bound(
                ToolCommand {
                    environment_policy: CommandEnvironmentPolicy::ClearAndSet,
                    binary: ToolBinary::AtomicParsley,
                    args: Vec::new(),
                    secret_args: Vec::new(),
                    cwd: None,
                    env: Vec::new(),
                    timeout: Duration::from_secs(5),
                },
                &authority,
                &CancellationToken::new(),
            )
            .await
            .expect_err("digest drift must be rejected");
        assert!(matches!(error, ToolRunnerError::Io(ref io) if io.kind() == std::io::ErrorKind::InvalidData));
    }

    #[cfg(unix)]
    #[test]
    fn real_runner_caches_detected_version() {
        let count_file = std::env::temp_dir().join(format!(
            "tonepoet-version-count-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&count_file);
        let script = write_executable_script(
            "fake-ffmpeg",
            &format!(
                r#"#!/bin/sh
COUNT='{}'
if [ -f "$COUNT" ]; then n=$(cat "$COUNT"); else n=0; fi
n=$((n + 1))
printf '%s\n' "$n" > "$COUNT"
printf 'ffmpeg version 9.9.%s\n' "$n"
"#,
                count_file.display()
            ),
        );
        let runner = runner_with_override(ToolBinary::Ffmpeg, script.to_str().unwrap());

        assert_eq!(runner.tool_version(ToolBinary::Ffmpeg), Some("9.9.1".to_string()));
        assert_eq!(runner.tool_version(ToolBinary::Ffmpeg), Some("9.9.1".to_string()));
        assert_eq!(
            std::fs::read_to_string(&count_file).expect("count file").trim(),
            "1"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn real_runner_detects_version_on_first_run_and_reuses_cache() {
        let count_file = std::env::temp_dir().join(format!(
            "tonepoet-first-use-version-count-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&count_file);
        let script = write_executable_script(
            "fake-ffmpeg-first-use",
            &format!(
                r#"#!/bin/sh
COUNT='{}'
if [ "$1" = "--version" ]; then
  if [ -f "$COUNT" ]; then n=$(cat "$COUNT"); else n=0; fi
  n=$((n + 1))
  printf '%s\n' "$n" > "$COUNT"
  printf 'ffmpeg version 12.3.4\n'
  exit 0
fi
printf 'actual run\n'
exit 0
"#,
                count_file.display()
            ),
        );
        let runner = runner_with_override(ToolBinary::Ffmpeg, script.to_str().unwrap());
        let cancel = CancellationToken::new();
        let command = || ToolCommand {
            environment_policy: tonepoet_pipeline::CommandEnvironmentPolicy::InheritAndSet,
            binary: ToolBinary::Ffmpeg,
            args: vec!["encode".to_string()],
            secret_args: vec![],
            cwd: None,
            env: vec![],
            timeout: Duration::from_secs(5),
        };

        runner
            .run(command(), &cancel)
            .await
            .expect("first command succeeds");
        runner
            .run(command(), &cancel)
            .await
            .expect("second command succeeds");

        assert_eq!(runner.tool_version(ToolBinary::Ffmpeg), Some("12.3.4".to_string()));
        assert_eq!(
            std::fs::read_to_string(&count_file).expect("count file").trim(),
            "1"
        );
    }

    #[cfg(unix)]
    #[test]
    fn real_runner_caches_failed_detection_as_none() {
        let count_file = std::env::temp_dir().join(format!(
            "tonepoet-version-fail-count-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&count_file);
        let script = write_executable_script(
            "fake-ffmpeg-no-version",
            &format!(
                r#"#!/bin/sh
COUNT='{}'
if [ -f "$COUNT" ]; then n=$(cat "$COUNT"); else n=0; fi
n=$((n + 1))
printf '%s\n' "$n" > "$COUNT"
printf 'ffmpeg build metadata unavailable\n'
"#,
                count_file.display()
            ),
        );
        let runner = runner_with_override(ToolBinary::Ffmpeg, script.to_str().unwrap());

        // Failed external detections are omitted from provenance by returning
        // None; they must not be mislabeled as "in-process".
        assert_eq!(runner.tool_version(ToolBinary::Ffmpeg), None);
        assert_ne!(
            runner.tool_version(ToolBinary::Ffmpeg),
            Some("in-process".to_string())
        );
        assert_eq!(
            std::fs::read_to_string(&count_file).expect("count file").trim(),
            "1"
        );
    }

    #[cfg(unix)]
    #[test]
    fn concurrent_first_use_runs_only_one_version_probe() {
        use std::sync::{Arc, Barrier};

        let count_file = std::env::temp_dir().join(format!(
            "tonepoet-version-concurrent-count-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&count_file);
        let script = write_executable_script(
            "fake-ffmpeg-concurrent-version",
            &format!(
                r#"#!/bin/sh
COUNT='{}'
if [ "$1" = "--version" ]; then
  printf 'probe\n' >> "$COUNT"
  printf 'ffmpeg version 55.66.77\n'
  exit 0
fi
printf 'actual run\n'
exit 0
"#,
                count_file.display()
            ),
        );
        let runner = Arc::new(runner_with_override(
            ToolBinary::Ffmpeg,
            script.to_str().unwrap(),
        ));
        let thread_count = 8;
        let barrier = Arc::new(Barrier::new(thread_count));
        let mut handles = Vec::new();

        for _ in 0..thread_count {
            let runner = Arc::clone(&runner);
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                runner.tool_version(ToolBinary::Ffmpeg)
            }));
        }

        for handle in handles {
            assert_eq!(
                handle.join().expect("thread should not panic"),
                Some("55.66.77".to_string())
            );
        }

        let probe_count = std::fs::read_to_string(&count_file)
            .expect("count file")
            .lines()
            .count();
        assert_eq!(probe_count, 1);
    }

    #[cfg(unix)]
    #[test]
    fn version_probe_captures_verbose_output_without_pipe_backpressure() {
        let script = write_executable_script(
            "fake-sox-verbose-help",
            r#"#!/bin/sh
printf 'sox:      SoX_ng v14.6.1\n'
i=0
while [ $i -lt 3000 ]; do
  printf 'verbose help line from a very chatty help command %05d\n' "$i"
  i=$((i + 1))
done
exit 0
"#,
        );

        let started = std::time::Instant::now();
        let version = detect_tool_version(ToolBinary::Sox, &script);
        let elapsed = started.elapsed();

        assert_eq!(version, Some("14.6.1".to_string()));
        assert!(
            elapsed < Duration::from_millis(500),
            "version probe should avoid pipe backpressure and complete promptly, took {elapsed:?}"
        );
    }
}
