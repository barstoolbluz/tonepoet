//! PR 1 — external-tool execution contract.
//!
//! Defines the closed set of tools the pipeline may invoke, the
//! command/output types, the `ToolRunner` trait, and a transcript-
//! backed stub runner for materializer/orchestrator unit tests.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

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
}

// ===========================================================================
// Stub runner — PR 1
// ===========================================================================

/// Configured response for the next stub `run` call.
#[derive(Clone)]
enum StubResponse {
    Output(ToolOutput),
    Fail(String),
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

    /// The sanitized command transcript, in call order.
    pub fn transcript(&self) -> Vec<CommandRecord> {
        self.transcript.lock().unwrap().clone()
    }

    fn record(&self, cmd: &ToolCommand, exit: Option<ProcessExit>, stderr: &str) -> CommandRecord {
        CommandRecord {
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
}

impl RealToolRunner {
    /// Create a runner. `tool_paths` keys are canonical binary names
    /// (e.g. `"ffmpeg"`, `"sox"`) matching `ToolBinary::default_name()`.
    /// An empty map means all tools are resolved from `$PATH`.
    pub fn new(tool_paths: HashMap<String, PathBuf>) -> Self {
        Self { tool_paths }
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

#[async_trait]
impl ToolRunner for RealToolRunner {
    async fn run(
        &self,
        cmd: ToolCommand,
        cancel: &CancellationToken,
    ) -> Result<ToolOutput, ToolRunnerError> {
        let binary_path = self.resolve_binary(cmd.binary);
        let start = Instant::now();

        // Build the process command.
        let mut proc = tokio::process::Command::new(&binary_path);
        proc.args(&cmd.args)
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
