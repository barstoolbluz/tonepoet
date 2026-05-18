//! Streaming child-process execution with progress probes.
//!
//! This helper intentionally leaves the `ToolRunner` trait unchanged. It is used
//! only at call sites that need live stdout/stderr parsing. It returns the same
//! `ToolOutput` / `ToolRunnerError` shapes as `RealToolRunner` and uses the same
//! configured-path resolution rules when callers pass the processor's tool map.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::sync::mpsc;
use tokio::time::{self, Instant};
use tokio_util::sync::CancellationToken;

use super::heartbeat::DEFAULT_HEARTBEAT_INTERVAL;
use super::operation::OperationProgressTracker;
use crate::convert::pipeline::errors::ToolRunnerError;
use crate::convert::pipeline::tool::{
    CommandRecord, ProcessExit, ToolBinary, ToolCommand, ToolOutput, TOOL_OUTPUT_TAIL_BYTES,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamSource {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ProbeUpdate {
    Measured {
        progress: f32,
        material_key: String,
        message: String,
    },
    Unknown {
        material_key: String,
        message: String,
    },
}

impl ProbeUpdate {
    pub fn measured(progress: f32, material_key: String, message: String) -> Self {
        Self::Measured {
            progress: progress.clamp(0.0, 1.0),
            material_key,
            message,
        }
    }

    pub fn unknown(material_key: String, message: String) -> Self {
        Self::Unknown {
            material_key,
            message,
        }
    }

    pub fn progress(&self) -> f32 {
        match self {
            Self::Measured { progress, .. } => *progress,
            Self::Unknown { .. } => 0.0,
        }
    }

    pub fn message(&self) -> &str {
        match self {
            Self::Measured { message, .. } | Self::Unknown { message, .. } => message,
        }
    }

    pub fn is_unknown(&self) -> bool {
        matches!(self, Self::Unknown { .. })
    }
}

#[derive(Debug, Clone)]
pub struct StreamingHeartbeat {
    pub interval: Duration,
    pub material_key: String,
    pub message: String,
}

impl StreamingHeartbeat {
    pub fn new(material_key: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            interval: DEFAULT_HEARTBEAT_INTERVAL,
            material_key: material_key.into(),
            message: message.into(),
        }
    }

    pub fn with_interval(mut self, interval: Duration) -> Self {
        self.interval = interval;
        self
    }
}

#[derive(Debug)]
struct StreamLine {
    source: StreamSource,
    line: String,
}

/// Run a command with streaming probes using the same default resolution rules
/// as `RealToolRunner` when no configured tool-path map is available.
pub async fn run_streaming_tool_with_probe<F>(
    cmd: ToolCommand,
    cancel: &CancellationToken,
    tracker: Option<&mut OperationProgressTracker<'_>>,
    heartbeat: Option<StreamingHeartbeat>,
    parse_line: F,
) -> Result<ToolOutput, ToolRunnerError>
where
    F: FnMut(StreamSource, &str) -> Option<ProbeUpdate>,
{
    let tool_paths = HashMap::new();
    run_streaming_tool_with_probe_with_tool_paths(
        cmd,
        cancel,
        tracker,
        heartbeat,
        &tool_paths,
        parse_line,
    )
    .await
}

/// Run a command with streaming probes and configured-path resolution.
///
/// This mirrors `RealToolRunner::resolve_binary`: configured overrides win,
/// then 7z can use the platform detector, then the canonical binary name is
/// used from `$PATH`.
pub async fn run_streaming_tool_with_probe_with_tool_paths<F>(
    cmd: ToolCommand,
    cancel: &CancellationToken,
    tracker: Option<&mut OperationProgressTracker<'_>>,
    heartbeat: Option<StreamingHeartbeat>,
    tool_paths: &HashMap<String, PathBuf>,
    parse_line: F,
) -> Result<ToolOutput, ToolRunnerError>
where
    F: FnMut(StreamSource, &str) -> Option<ProbeUpdate>,
{
    let binary_path = resolve_binary_with_tool_paths(cmd.binary, tool_paths);
    run_streaming_tool_with_probe_at_path(binary_path, cmd, cancel, tracker, heartbeat, parse_line)
        .await
}

pub(crate) async fn run_streaming_tool_with_probe_at_path<F>(
    binary_path: PathBuf,
    cmd: ToolCommand,
    cancel: &CancellationToken,
    mut tracker: Option<&mut OperationProgressTracker<'_>>,
    heartbeat: Option<StreamingHeartbeat>,
    mut parse_line: F,
) -> Result<ToolOutput, ToolRunnerError>
where
    F: FnMut(StreamSource, &str) -> Option<ProbeUpdate>,
{
    let start = Instant::now();

    let mut proc = tokio::process::Command::new(&binary_path);
    proc.args(&cmd.args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(ref cwd) = cmd.cwd {
        proc.current_dir(cwd);
    }
    for env_var in &cmd.env {
        proc.env(&env_var.key, env_var.value.expose());
    }

    let mut child = proc.spawn().map_err(|_io| {
        let elapsed = start.elapsed();
        ToolRunnerError::Spawn {
            command: build_record(&cmd, None, "", "", elapsed),
        }
    })?;

    let (line_tx, mut line_rx) = mpsc::channel::<StreamLine>(32);
    let stdout_task = child
        .stdout
        .take()
        .map(|reader| spawn_stream_reader(StreamSource::Stdout, reader, line_tx.clone()));
    let stderr_task = child
        .stderr
        .take()
        .map(|reader| spawn_stream_reader(StreamSource::Stderr, reader, line_tx.clone()));
    drop(line_tx);

    let timeout_sleep = time::sleep(cmd.timeout);
    tokio::pin!(timeout_sleep);
    let heartbeat_interval = heartbeat
        .as_ref()
        .map(|heartbeat| heartbeat.interval)
        .unwrap_or(DEFAULT_HEARTBEAT_INTERVAL);
    let mut heartbeat_ticks =
        time::interval_at(Instant::now() + heartbeat_interval, heartbeat_interval);

    let wait_result = loop {
        tokio::select! {
            status = child.wait() => {
                break match status {
                    Ok(status) => Ok(status),
                    Err(err) => Err(err),
                };
            }
            _ = &mut timeout_sleep => {
                let _ = child.start_kill();
                let _ = child.wait().await;
                let elapsed = start.elapsed();
                let (stdout_tail, stderr_tail) = collect_tails_while_draining(
                    stdout_task,
                    stderr_task,
                    &mut line_rx,
                    &mut tracker,
                    &mut parse_line,
                )
                .await;
                return Err(ToolRunnerError::Timeout {
                    elapsed,
                    command: build_record(&cmd, None, &stdout_tail, &stderr_tail, elapsed),
                });
            }
            _ = cancel.cancelled() => {
                if let Some(tracker) = tracker.as_deref_mut() {
                    tracker.cancel_requested().await;
                }
                let _ = child.start_kill();
                let _ = child.wait().await;
                let elapsed = start.elapsed();
                let (stdout_tail, stderr_tail) = collect_tails_while_draining(
                    stdout_task,
                    stderr_task,
                    &mut line_rx,
                    &mut tracker,
                    &mut parse_line,
                )
                .await;
                return Err(ToolRunnerError::Cancelled {
                    command: build_record(&cmd, None, &stdout_tail, &stderr_tail, elapsed),
                });
            }
            Some(line) = line_rx.recv() => {
                apply_line(&mut tracker, &mut parse_line, line).await;
            }
            _ = heartbeat_ticks.tick(), if heartbeat.is_some() => {
                if let (Some(heartbeat), Some(tracker)) = (heartbeat.as_ref(), tracker.as_deref_mut()) {
                    tracker
                        .unknown_alive_with_key(&heartbeat.material_key, &heartbeat.message)
                        .await;
                }
            }
        }
    };

    let elapsed = start.elapsed();
    // The child can exit before the select loop receives the last queued line.
    // Keep draining progress lines while waiting for reader tasks to finish;
    // otherwise a fast, chatty process can fill the bounded channel and block a
    // reader task, which would make tail collection wait forever.
    let (stdout_tail, stderr_tail) = collect_tails_while_draining(
        stdout_task,
        stderr_task,
        &mut line_rx,
        &mut tracker,
        &mut parse_line,
    )
    .await;

    match wait_result {
        Ok(status) => {
            let exit = map_exit_status(status);
            let record = build_record(&cmd, Some(exit), &stdout_tail, &stderr_tail, elapsed);
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
        Err(io_err) => Err(ToolRunnerError::Io(io_err)),
    }
}

async fn collect_tails_while_draining<F>(
    mut stdout_task: Option<tokio::task::JoinHandle<String>>,
    mut stderr_task: Option<tokio::task::JoinHandle<String>>,
    line_rx: &mut mpsc::Receiver<StreamLine>,
    tracker: &mut Option<&mut OperationProgressTracker<'_>>,
    parse_line: &mut F,
) -> (String, String)
where
    F: FnMut(StreamSource, &str) -> Option<ProbeUpdate>,
{
    let mut stdout_tail: Option<String> = None;
    let mut stderr_tail: Option<String> = None;

    loop {
        match (stdout_task.is_some(), stderr_task.is_some()) {
            (false, false) => {
                while let Some(line) = line_rx.recv().await {
                    apply_line(tracker, parse_line, line).await;
                }
                return (
                    stdout_tail.unwrap_or_default(),
                    stderr_tail.unwrap_or_default(),
                );
            }
            (true, true) => {
                tokio::select! {
                    Some(line) = line_rx.recv() => {
                        apply_line(tracker, parse_line, line).await;
                    }
                    result = stdout_task.as_mut().expect("stdout task present") => {
                        stdout_tail = Some(result.unwrap_or_default());
                        stdout_task = None;
                    }
                    result = stderr_task.as_mut().expect("stderr task present") => {
                        stderr_tail = Some(result.unwrap_or_default());
                        stderr_task = None;
                    }
                }
            }
            (true, false) => {
                tokio::select! {
                    Some(line) = line_rx.recv() => {
                        apply_line(tracker, parse_line, line).await;
                    }
                    result = stdout_task.as_mut().expect("stdout task present") => {
                        stdout_tail = Some(result.unwrap_or_default());
                        stdout_task = None;
                    }
                }
            }
            (false, true) => {
                tokio::select! {
                    Some(line) = line_rx.recv() => {
                        apply_line(tracker, parse_line, line).await;
                    }
                    result = stderr_task.as_mut().expect("stderr task present") => {
                        stderr_tail = Some(result.unwrap_or_default());
                        stderr_task = None;
                    }
                }
            }
        }
    }
}

async fn apply_line<F>(
    tracker: &mut Option<&mut OperationProgressTracker<'_>>,
    parse_line: &mut F,
    line: StreamLine,
) where
    F: FnMut(StreamSource, &str) -> Option<ProbeUpdate>,
{
    if let Some(update) = parse_line(line.source, &line.line) {
        if let Some(tracker) = tracker.as_deref_mut() {
            apply_probe_update(tracker, update).await;
        }
    }
}

async fn apply_probe_update(tracker: &mut OperationProgressTracker<'_>, update: ProbeUpdate) {
    match update {
        ProbeUpdate::Measured {
            progress,
            material_key,
            message,
        } => {
            tracker
                .measured_with_key(progress, material_key, message)
                .await;
        }
        ProbeUpdate::Unknown {
            material_key,
            message,
        } => {
            tracker.unknown_alive_with_key(material_key, message).await;
        }
    }
}

pub fn resolve_binary_with_tool_paths(
    binary: ToolBinary,
    tool_paths: &HashMap<String, PathBuf>,
) -> PathBuf {
    let name = binary.default_name();
    if let Some(path) = tool_paths.get(name) {
        return path.clone();
    }
    if binary == ToolBinary::SevenZip {
        if let Some(bin) = crate::detect_7z_binary() {
            return PathBuf::from(bin);
        }
    }
    PathBuf::from(name)
}

fn spawn_stream_reader<R>(
    source: StreamSource,
    reader: R,
    line_tx: mpsc::Sender<StreamLine>,
) -> tokio::task::JoinHandle<String>
where
    R: AsyncRead + Send + Unpin + 'static,
{
    tokio::spawn(async move {
        let mut reader = BufReader::new(reader);
        let mut tail = TailBuffer::new(TOOL_OUTPUT_TAIL_BYTES);
        let mut line = Vec::new();
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line).await {
                Ok(0) => break,
                Ok(_) => {
                    tail.push(&line);
                    let text = String::from_utf8_lossy(&line).trim_end().to_string();
                    let _ = line_tx.send(StreamLine { source, line: text }).await;
                }
                Err(_) => break,
            }
        }
        tail.into_string()
    })
}

struct TailBuffer {
    bytes: Vec<u8>,
    limit: usize,
}

impl TailBuffer {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
        }
    }

    fn push(&mut self, chunk: &[u8]) {
        if self.limit == 0 {
            self.bytes.clear();
            return;
        }
        if chunk.len() >= self.limit {
            self.bytes.clear();
            self.bytes
                .extend_from_slice(&chunk[chunk.len().saturating_sub(self.limit)..]);
            return;
        }
        self.bytes.extend_from_slice(chunk);
        let overflow = self.bytes.len().saturating_sub(self.limit);
        if overflow > 0 {
            self.bytes.drain(..overflow);
        }
    }

    fn into_string(self) -> String {
        String::from_utf8_lossy(&self.bytes).into_owned()
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convert::pipeline::reporter::{PipelineEvent, RecordingReporter};
    use crate::convert::pipeline::types::PipelineStage;
    use std::sync::{Arc, Mutex};

    fn sh_command(script: &str, timeout_secs: u64) -> ToolCommand {
        ToolCommand {
            binary: ToolBinary::Ffmpeg,
            args: vec!["-c".into(), script.into()],
            secret_args: vec![],
            cwd: None,
            env: vec![],
            timeout: Duration::from_secs(timeout_secs),
        }
    }

    fn progress_events(reporter: &RecordingReporter) -> Vec<PipelineEvent> {
        reporter
            .events()
            .into_iter()
            .filter(|event| matches!(event, PipelineEvent::Progress { .. }))
            .collect()
    }

    #[tokio::test]
    async fn streaming_helper_feeds_probe_updates_through_tracker() {
        let reporter = RecordingReporter::new();
        let mut tracker =
            OperationProgressTracker::new("item-1", PipelineStage::Convert, Some(&reporter));
        let cmd = sh_command("echo 'time=00:00:01.00' >&2", 5);
        let cancel = CancellationToken::new();
        let result = run_streaming_tool_with_probe_at_path(
            PathBuf::from("/bin/sh"),
            cmd,
            &cancel,
            Some(&mut tracker),
            None,
            |source, line| {
                if source == StreamSource::Stderr && line.contains("time=") {
                    Some(ProbeUpdate::measured(
                        0.5,
                        "ffmpeg-progress".to_string(),
                        "Encoding · 50%".to_string(),
                    ))
                } else {
                    None
                }
            },
        )
        .await;
        assert!(result.is_ok());
        assert_eq!(progress_events(&reporter).len(), 1);
    }

    #[tokio::test]
    async fn streaming_helper_drains_final_queued_lines_after_child_exit() {
        let reporter = RecordingReporter::new();
        let mut tracker =
            OperationProgressTracker::new("item-1", PipelineStage::Convert, Some(&reporter));
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let seen_for_parser = Arc::clone(&seen);
        let cmd = sh_command("for i in 1 2 3 4 5; do echo progress-$i >&2; done", 5);
        let cancel = CancellationToken::new();
        let result = run_streaming_tool_with_probe_at_path(
            PathBuf::from("/bin/sh"),
            cmd,
            &cancel,
            Some(&mut tracker),
            None,
            move |source, line| {
                if source == StreamSource::Stderr {
                    seen_for_parser.lock().unwrap().push(line.to_string());
                    if line == "progress-5" {
                        return Some(ProbeUpdate::measured(
                            1.0,
                            "final-line".to_string(),
                            "Final line observed".to_string(),
                        ));
                    }
                }
                None
            },
        )
        .await;
        assert!(result.is_ok());
        assert!(seen.lock().unwrap().iter().any(|line| line == "progress-5"));
        assert_eq!(progress_events(&reporter).len(), 1);
    }

    #[tokio::test]
    async fn streaming_helper_drains_more_lines_than_channel_capacity_after_child_exit() {
        let reporter = RecordingReporter::new();
        let mut tracker =
            OperationProgressTracker::new("item-1", PipelineStage::Convert, Some(&reporter));
        let seen = Arc::new(Mutex::new(0_usize));
        let seen_for_parser = Arc::clone(&seen);
        let cmd = sh_command("for i in $(seq 1 96); do echo progress-$i >&2; done", 5);
        let cancel = CancellationToken::new();
        let result = run_streaming_tool_with_probe_at_path(
            PathBuf::from("/bin/sh"),
            cmd,
            &cancel,
            Some(&mut tracker),
            None,
            move |source, line| {
                if source == StreamSource::Stderr {
                    *seen_for_parser.lock().unwrap() += 1;
                    if line == "progress-96" {
                        return Some(ProbeUpdate::measured(
                            1.0,
                            "last-stress-line".to_string(),
                            "Last stress line observed".to_string(),
                        ));
                    }
                }
                None
            },
        )
        .await;
        assert!(result.is_ok());
        assert_eq!(*seen.lock().unwrap(), 96);
        assert_eq!(progress_events(&reporter).len(), 1);
    }

    #[tokio::test]
    async fn streaming_helper_handles_timeout() {
        let cmd = sh_command("sleep 60", 1);
        let cancel = CancellationToken::new();
        let err = run_streaming_tool_with_probe_at_path(
            PathBuf::from("/bin/sh"),
            cmd,
            &cancel,
            None,
            None,
            |_source, _line| None,
        )
        .await
        .expect_err("should timeout");
        assert!(matches!(err, ToolRunnerError::Timeout { .. }));
    }

    #[tokio::test]
    async fn streaming_helper_handles_cancellation() {
        let cmd = sh_command("sleep 60", 30);
        let cancel = CancellationToken::new();
        let cancel2 = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            cancel2.cancel();
        });
        let err = run_streaming_tool_with_probe_at_path(
            PathBuf::from("/bin/sh"),
            cmd,
            &cancel,
            None,
            None,
            |_source, _line| None,
        )
        .await
        .expect_err("should cancel");
        assert!(matches!(err, ToolRunnerError::Cancelled { .. }));
    }

    #[test]
    fn configured_tool_path_overrides_default_resolution() {
        let mut tool_paths = HashMap::new();
        tool_paths.insert("ffmpeg".to_string(), PathBuf::from("/custom/ffmpeg"));
        assert_eq!(
            resolve_binary_with_tool_paths(ToolBinary::Ffmpeg, &tool_paths),
            PathBuf::from("/custom/ffmpeg")
        );
    }

    #[test]
    fn falls_back_to_default_name_without_configured_path() {
        let tool_paths = HashMap::new();
        assert_eq!(
            resolve_binary_with_tool_paths(ToolBinary::Sox, &tool_paths),
            PathBuf::from("sox")
        );
    }
}
