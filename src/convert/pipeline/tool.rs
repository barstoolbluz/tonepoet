//! PR 1 — external-tool execution contract.
//!
//! Defines the closed set of tools the pipeline may invoke, the
//! command/output types, the `ToolRunner` trait, and a transcript-
//! backed stub runner for materializer/orchestrator unit tests.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
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
    Loudgain,
    Metaflac,
    Opustags,
    Wvunpack,
    Wvtag,
    AtomicParsley,
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
        self.responses.lock().unwrap().push(StubResponse::Output(output));
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
