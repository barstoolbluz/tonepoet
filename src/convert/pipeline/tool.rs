//! PR 1 — external-tool execution contract.
//!
//! Defines the closed set of tools the pipeline may invoke, the
//! command/output types, the `ToolRunner` trait, and a transcript-
//! backed stub runner for materializer/orchestrator unit tests.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant as StdInstant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

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
    pub env: Vec<EnvVar>,
    pub timeout: Duration,
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

    /// Env var keys only — values are never copied into diagnostics.
    pub fn env_keys(&self) -> Vec<String> {
        self.env.iter().map(|e| e.key.clone()).collect()
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

/// Maximum stdout/stderr tail any runner retains.
pub const TOOL_OUTPUT_TAIL_BYTES: usize = 64 * 1024;

#[async_trait]
pub trait ToolRunner: Send + Sync {
    async fn run(
        &self,
        cmd: ToolCommand,
        cancel: &CancellationToken,
    ) -> Result<ToolOutput, ToolRunnerError>;

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
        }
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
        ToolBinary::Sox | ToolBinary::Opustags => &["--help"],
        ToolBinary::Ffmpeg
        | ToolBinary::Ffprobe
        | ToolBinary::Loudgain
        | ToolBinary::Metaflac
        | ToolBinary::Flac
        | ToolBinary::Wvunpack
        | ToolBinary::Wvtag
        | ToolBinary::AtomicParsley => &["--version"],
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

fn parse_tool_version_output(binary: ToolBinary, stdout: &str, stderr: &str) -> Option<String> {
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
            | ToolBinary::AtomicParsley => first_version_like_token(line),
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

    let spawn_result = std::process::Command::new(path)
        .args(version_command_args(binary))
        // Same rule as every other subprocess (see the archive-hang fix): a
        // tool that reads stdin on a version-ish invocation must get EOF, not
        // an inherited terminal — a blocked probe wedges the version cache's
        // in-progress marker and stalls every waiter.
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(stderr_file))
        .spawn();

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

fn executable_path_is_available(path: &Path) -> bool {
    let is_usable_file = |candidate: &Path| {
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
    };

    if path.components().count() > 1 || path.is_absolute() {
        return is_usable_file(path);
    }
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|dir| is_usable_file(&dir.join(path))))
        .unwrap_or(false)
}

#[async_trait]
impl ToolRunner for RealToolRunner {
    fn tool_version(&self, binary: ToolBinary) -> Option<String> {
        let path = self.resolve_binary(binary);
        self.tool_version_for_resolved_path(binary, &path)
    }

    fn tool_available(&self, binary: ToolBinary) -> bool {
        executable_path_is_available(&self.resolve_binary(binary))
    }

    async fn run(
        &self,
        cmd: ToolCommand,
        cancel: &CancellationToken,
    ) -> Result<ToolOutput, ToolRunnerError> {
        // Capture the external tool's real version on first use, before the
        // command itself is spawned. This is deliberately best-effort and
        // non-fatal: failed probes cache an empty value so conversion does not
        // block or retry repeatedly. Keep this outside the command timer so
        // provenance collection does not inflate the recorded runtime of the
        // user's actual tool invocation.
        let _ = self.tool_version(cmd.binary);

        let binary_path = self.resolve_binary(cmd.binary);
        let start = Instant::now();

        // Build the process command.
        let mut proc = tokio::process::Command::new(&binary_path);
        proc.args(&cmd.args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        if let Some(ref cwd) = cmd.cwd {
            proc.current_dir(cwd);
        }
        for env_var in &cmd.env {
            proc.env(&env_var.key, env_var.value.expose());
        }

        // Spawn.
        let mut child = proc.spawn().map_err(|_io| {
            let elapsed = start.elapsed();
            ToolRunnerError::Spawn {
                command: Self::build_record(&cmd, None, "", "", elapsed),
            }
        })?;

        // Take pipe handles so we can read them concurrently.
        let stdout_pipe = child.stdout.take();
        let stderr_pipe = child.stderr.take();

        let stdout_task = tokio::spawn(async move {
            match stdout_pipe {
                Some(r) => read_tail(r).await,
                None => String::new(),
            }
        });
        let stderr_task = tokio::spawn(async move {
            match stderr_pipe {
                Some(r) => read_tail(r).await,
                None => String::new(),
            }
        });

        enum WaitOutcome {
            Finished(Result<std::process::ExitStatus, std::io::Error>),
            TimedOut,
            Cancelled,
        }

        // Race child completion, timeout, and cancellation. Always reap the
        // child and join pipe readers before returning so cancelled or timed-out
        // runs do not leave detached stdout/stderr tasks behind.
        let wait_outcome = tokio::select! {
            status = child.wait() => WaitOutcome::Finished(status),
            _ = tokio::time::sleep(cmd.timeout) => {
                let _ = child.start_kill();
                let _ = child.wait().await;
                WaitOutcome::TimedOut
            }
            _ = cancel.cancelled() => {
                let _ = child.start_kill();
                let _ = child.wait().await;
                WaitOutcome::Cancelled
            }
        };

        let elapsed = start.elapsed();

        // Collect bounded stdout/stderr tails after the process exits or is
        // killed. The readers retain at most TOOL_OUTPUT_TAIL_BYTES each.
        let stdout_tail = stdout_task.await.unwrap_or_default();
        let stderr_tail = stderr_task.await.unwrap_or_default();

        match wait_outcome {
            WaitOutcome::Finished(Ok(status)) => {
                let exit = map_exit_status(status);
                let record =
                    Self::build_record(&cmd, Some(exit), &stdout_tail, &stderr_tail, elapsed);

                if status.success() {
                    Ok(ToolOutput {
                        exit,
                        stdout_tail,
                        stderr_tail,
                        elapsed,
                        command: record,
                    })
                } else {
                    Err(ToolRunnerError::NonZeroExit {
                        exit,
                        stderr_tail,
                        command: record,
                    })
                }
            }
            WaitOutcome::Finished(Err(io_err)) => Err(ToolRunnerError::Io(io_err)),
            WaitOutcome::TimedOut => Err(ToolRunnerError::Timeout {
                elapsed,
                command: Self::build_record(&cmd, None, &stdout_tail, &stderr_tail, elapsed),
            }),
            WaitOutcome::Cancelled => Err(ToolRunnerError::Cancelled {
                command: Self::build_record(&cmd, None, &stdout_tail, &stderr_tail, elapsed),
            }),
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
                binary,
                args: vec![arg.to_string()],
                secret_args: Vec::new(),
                cwd: None,
                env: Vec::new(),
                timeout: Duration::from_secs(60),
            }
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

    #[tokio::test]
    async fn timeout_kills_hung_process() {
        let runner = runner_with_override(ToolBinary::Ffmpeg, "sleep");
        let cancel = CancellationToken::new();
        let cmd = ToolCommand {
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
