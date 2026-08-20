//! Streaming child-process execution with progress probes.
//!
//! This helper intentionally leaves the `ToolRunner` trait unchanged. It is used
//! only at call sites that need live stdout/stderr parsing. It returns the same
//! `ToolOutput` / `ToolRunnerError` shapes as `RealToolRunner` and uses the same
//! configured-path resolution rules when callers pass the processor's tool map.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::mpsc;
use tokio::time::{self, Instant};
use tokio_util::sync::CancellationToken;

use super::heartbeat::DEFAULT_HEARTBEAT_INTERVAL;
use super::operation::OperationProgressTracker;
use crate::convert::pipeline::errors::ToolRunnerError;
use crate::convert::pipeline::tool::{
    resolve_command_launch_path, ToolBinary, ToolCommand, ToolOutput,
    TOOL_OUTPUT_TAIL_BYTES, TOOL_TERMINATION_TIMEOUT,
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
    let binary_path = resolve_command_launch_path(
        resolve_binary_with_tool_paths(cmd.binary, tool_paths),
        cmd.environment_policy,
    )
    .map_err(ToolRunnerError::Io)?;
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
    #[cfg(unix)]
    {
        use std::os::fd::FromRawFd;
        use std::sync::Arc;
        use crate::convert::pipeline::tool::RealToolRunner;

        // Live progress uses ordinary CLOEXEC stdio pipes. The command itself
        // still executes under the trusted tonepoet supervisor, which alone
        // retains QueueExecution/path/staging lease OFDs.
        #[allow(unsafe_code)] // raw pipe2 + from_raw_fd for lease-lifetime fd inheritance into the worker
        fn pipe_pair() -> Result<(std::fs::File, Arc<std::fs::File>), ToolRunnerError> {
            let mut fds = [-1_i32; 2];
            if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
                return Err(ToolRunnerError::Io(std::io::Error::last_os_error()));
            }
            // SAFETY: pipe2 returned two newly-owned descriptors.
            let read = unsafe { std::fs::File::from_raw_fd(fds[0]) };
            let write = Arc::new(unsafe { std::fs::File::from_raw_fd(fds[1]) });
            Ok((read, write))
        }

        let (stdout_read, stdout_write) = pipe_pair()?;
        let (stderr_read, stderr_write) = pipe_pair()?;
        let (line_tx, mut line_rx) = mpsc::channel::<StreamLine>(32);
        let stdout_task = Some(spawn_stream_reader(
            StreamSource::Stdout,
            tokio::fs::File::from_std(stdout_read),
            line_tx.clone(),
        ));
        let stderr_task = Some(spawn_stream_reader(
            StreamSource::Stderr,
            tokio::fs::File::from_std(stderr_read),
            line_tx.clone(),
        ));
        drop(line_tx);

        let heartbeat_interval = heartbeat
            .as_ref()
            .map(|heartbeat| heartbeat.interval)
            .unwrap_or(DEFAULT_HEARTBEAT_INTERVAL);
        let mut heartbeat_ticks =
            time::interval_at(Instant::now() + heartbeat_interval, heartbeat_interval);

        let runner = RealToolRunner::new(HashMap::new());
        let run_future = runner.run_supervised_with_stdio(
            cmd.clone(),
            binary_path,
            cancel,
            None,
            Some(stdout_write),
            Some(stderr_write),
        );
        tokio::pin!(run_future);

        let result = loop {
            tokio::select! {
                result = &mut run_future => break result,
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

        let (stdout_tail, stderr_tail) = collect_tails_while_draining(
            stdout_task,
            stderr_task,
            &mut line_rx,
            &mut tracker,
            &mut parse_line,
        )
        .await;

        // The supervisor cannot capture custom-pipe tails itself. Preserve the
        // public streaming runner contract by installing the locally captured
        // bounded tails into both success and structured error records.
        match result {
            Ok(mut output) => {
                output.stdout_tail = stdout_tail.clone();
                output.stderr_tail = stderr_tail.clone();
                output.command.stdout_tail = stdout_tail;
                output.command.stderr_tail = stderr_tail;
                Ok(output)
            }
            Err(ToolRunnerError::NonZeroExit { exit, mut command, .. }) => {
                command.stdout_tail = stdout_tail;
                command.stderr_tail = stderr_tail.clone();
                Err(ToolRunnerError::NonZeroExit { exit, stderr_tail, command })
            }
            Err(ToolRunnerError::Timeout { elapsed, mut command }) => {
                command.stdout_tail = stdout_tail;
                command.stderr_tail = stderr_tail;
                Err(ToolRunnerError::Timeout { elapsed, command })
            }
            Err(ToolRunnerError::Cancelled { mut command }) => {
                command.stdout_tail = stdout_tail;
                command.stderr_tail = stderr_tail;
                if let Some(tracker) = tracker.as_deref_mut() {
                    tracker.cancel_requested_for_tool(cmd.binary.default_name()).await;
                    tracker.cancelled_at_last_progress().await;
                }
                Err(ToolRunnerError::Cancelled { command })
            }
            Err(ToolRunnerError::Termination { message, mut command }) => {
                command.stdout_tail = stdout_tail;
                command.stderr_tail = stderr_tail;
                Err(ToolRunnerError::Termination { message, command })
            }
            Err(other) => Err(other),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (binary_path, cmd, cancel, tracker, heartbeat, parse_line);
        Err(ToolRunnerError::Io(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "streaming mutation-capable tools require tonepoet supervision on this platform",
        )))
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
    let deadline = Instant::now() + TOOL_TERMINATION_TIMEOUT;

    loop {
        if Instant::now() >= deadline {
            break;
        }
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
                    _ = time::sleep_until(deadline) => break,
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
                    _ = time::sleep_until(deadline) => break,
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
                    _ = time::sleep_until(deadline) => break,
                }
            }
        }
    }

    if let Some(task) = stdout_task.take() {
        task.abort();
        let _ = task.await;
    }
    if let Some(task) = stderr_task.take() {
        task.abort();
        let _ = task.await;
    }
    while let Ok(line) = line_rx.try_recv() {
        apply_line(tracker, parse_line, line).await;
    }
    (
        stdout_tail.unwrap_or_default(),
        stderr_tail.unwrap_or_default(),
    )
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
        let mut reader = reader;
        let mut tail = TailBuffer::new(TOOL_OUTPUT_TAIL_BYTES);
        let mut line_buf = Vec::new();
        let mut chunk = [0u8; 4096];

        // Read in small chunks and split on both '\r' and '\n'. Sox -S uses
        // '\r' exclusively for progress updates (no '\n' until exit), so
        // read_until(b'\n') would buffer all progress lines until EOF.
        // Reading raw chunks and scanning for either delimiter ensures
        // progress probes see updates in real time.
        loop {
            match reader.read(&mut chunk).await {
                Ok(0) => {
                    // EOF — flush any remaining partial line
                    if !line_buf.is_empty() {
                        tail.push(&line_buf);
                        let text = String::from_utf8_lossy(&line_buf).to_string();
                        if !text.is_empty() {
                            let _ = line_tx.send(StreamLine { source, line: text }).await;
                        }
                    }
                    break;
                }
                Ok(n) => {
                    for &byte in &chunk[..n] {
                        if byte == b'\r' || byte == b'\n' {
                            if !line_buf.is_empty() {
                                tail.push(&line_buf);
                                let text = String::from_utf8_lossy(&line_buf).to_string();
                                if !text.is_empty() {
                                    let _ = line_tx.send(StreamLine { source, line: text }).await;
                                }
                                line_buf.clear();
                            }
                        } else {
                            line_buf.push(byte);
                        }
                    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convert::pipeline::reporter::{PipelineEvent, RecordingReporter};
    use crate::convert::pipeline::types::PipelineStage;
    use std::sync::{Arc, Mutex};

    fn sh_command(script: &str, timeout_secs: u64) -> ToolCommand {
        ToolCommand {
            environment_policy: tonepoet_pipeline::CommandEnvironmentPolicy::InheritAndSet,
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

    fn progress_messages(reporter: &RecordingReporter) -> Vec<String> {
        progress_events(reporter)
            .into_iter()
            .map(|event| match event {
                PipelineEvent::Progress { message, .. } => message.expect("message"),
                _ => unreachable!(),
            })
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
        let cmd = sh_command("exec sleep 5", 3);
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

        match err {
            ToolRunnerError::Timeout { command, .. } => assert!(command.exit.is_some()),
            other => panic!("expected timeout, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn streaming_helper_handles_cancellation() {
        let cmd = sh_command("exec sleep 5", 10);
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

        match err {
            ToolRunnerError::Cancelled { command } => assert!(command.exit.is_some()),
            other => panic!("expected cancellation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn streaming_clear_and_set_environment_excludes_ambient_values() {
        let mut cmd = sh_command(
            r#"printf 'home=%s path=%s lc=%s\n' "${HOME-unset}" "${PATH-unset}" "${LC_ALL-unset}""#,
            5,
        );
        cmd.environment_policy = tonepoet_pipeline::CommandEnvironmentPolicy::ClearAndSet;
        cmd.env.push(crate::convert::pipeline::tool::EnvVar {
            key: "LC_ALL".to_string(),
            value: crate::convert::pipeline::types::SecretString::new("C"),
            secret: false,
        });
        let output = run_streaming_tool_with_probe_at_path(
            PathBuf::from("/bin/sh"),
            cmd,
            &CancellationToken::new(),
            None,
            None,
            |_source, _line| None,
        )
        .await
        .expect("closed streaming command succeeds");
        // A cleared shell self-assigns the libc default PATH; assert
        // clearing via HOME/ambient-PATH absence and the LC_ALL allowlist.
        let tail = output.stdout_tail.trim().to_string();
        assert!(tail.starts_with("home=unset path="), "{tail}");
        assert!(tail.ends_with("lc=C"), "{tail}");
        assert!(
            !tail.contains(&std::env::var("PATH").unwrap_or_default()),
            "ambient PATH leaked into the cleared streaming child: {tail}"
        );
        assert_eq!(
            output.command.environment_policy,
            tonepoet_pipeline::CommandEnvironmentPolicy::ClearAndSet
        );
        assert_eq!(
            output.command.environment,
            std::collections::BTreeMap::from([("LC_ALL".to_string(), "C".to_string())])
        );
    }

    #[tokio::test]
    async fn cancel_during_streaming_tool_emits_tool_specific_message() {
        let reporter = RecordingReporter::new();
        let mut tracker =
            OperationProgressTracker::new("item-1", PipelineStage::Convert, Some(&reporter));
        let cmd = sh_command("exec sleep 5", 10);
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
            Some(&mut tracker),
            None,
            |_source, _line| None,
        )
        .await
        .expect_err("should cancel");

        assert!(matches!(err, ToolRunnerError::Cancelled { .. }));
        assert!(progress_messages(&reporter)
            .iter()
            .any(|message| message == "Stopping ffmpeg…"));
    }

    #[tokio::test]
    async fn cancelled_stream_preserves_last_known_progress_percentage() {
        let reporter = RecordingReporter::new();
        let mut tracker =
            OperationProgressTracker::new("item-1", PipelineStage::Convert, Some(&reporter));
        let cmd = sh_command("echo progress >&2; exec sleep 5", 10);
        let cancel = CancellationToken::new();
        let cancel2 = cancel.clone();
        let progress_seen = std::sync::Arc::new(tokio::sync::Notify::new());
        let wait_for_progress = std::sync::Arc::clone(&progress_seen);
        let cancel_task = tokio::spawn(async move {
            let ready = tokio::time::timeout(
                Duration::from_secs(10),
                wait_for_progress.notified(),
            )
            .await
            .map_err(|_| "streaming child did not publish measured progress before readiness deadline")
            .map(|_| ());
            // Always release the child even when readiness fails so a failed
            // regression never leaves the fixture sleeping until command timeout.
            cancel2.cancel();
            ready
        });

        let err = run_streaming_tool_with_probe_at_path(
            PathBuf::from("/bin/sh"),
            cmd,
            &cancel,
            Some(&mut tracker),
            None,
            |source, line| {
                if source == StreamSource::Stderr && line == "progress" {
                    progress_seen.notify_one();
                    Some(ProbeUpdate::measured(
                        0.37,
                        "ffmpeg-progress".to_string(),
                        "Encoding · 37%".to_string(),
                    ))
                } else {
                    None
                }
            },
        )
        .await
        .expect_err("should cancel");

        cancel_task
            .await
            .expect("progress readiness task must not panic")
            .expect("streaming progress must be observed before cancellation");
        assert!(matches!(err, ToolRunnerError::Cancelled { .. }));
        let messages = progress_messages(&reporter);
        assert!(messages
            .iter()
            .any(|message| message.starts_with("Cancelled at 37%")));
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
