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
}
