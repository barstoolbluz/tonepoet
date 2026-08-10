//! Host clipboard integration with bounded workers, observable outcomes, and diagnostics.
//!
//! Tonepoet's in-process text clipboard remains authoritative. Host mirroring
//! is an asynchronous projection: writes are coalesced, every external action
//! has a two-second deadline, and failures are reported without changing the
//! success of the internal copy/cut operation.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::sync::mpsc;

use super::message::{AppMessage, HostClipboardPasteTarget};

const NATIVE_CLIPBOARD_MAX_BYTES: usize = 1024 * 1024;
const OSC52_TEXT_CLIPBOARD_MAX_BYTES: usize = 64 * 1024;
const CLIPBOARD_COMMAND_TIMEOUT: Duration = Duration::from_secs(2);
const CLIPBOARD_POLL_INTERVAL: Duration = Duration::from_millis(10);
const CLIPBOARD_HISTORY_LIMIT: usize = 32;

#[derive(Default)]
struct HostClipboardWriteState {
    pending: Option<String>,
    worker_running: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClipboardOperation {
    Write,
    Read,
    Diagnostic,
}

impl ClipboardOperation {
    fn label(self) -> &'static str {
        match self {
            Self::Write => "write",
            Self::Read => "read",
            Self::Diagnostic => "diagnostic",
        }
    }
}

#[derive(Debug, Clone)]
struct ClipboardAttempt {
    at: SystemTime,
    operation: ClipboardOperation,
    transport: String,
    outcome: Result<String, String>,
}

#[derive(Debug, Clone)]
struct ClipboardEnvironment {
    wayland_display: Option<OsString>,
    display: Option<OsString>,
    tmux: Option<OsString>,
    sty: Option<OsString>,
    term: Option<OsString>,
    path: Option<OsString>,
}

impl ClipboardEnvironment {
    fn detect() -> Self {
        Self {
            wayland_display: std::env::var_os("WAYLAND_DISPLAY"),
            display: std::env::var_os("DISPLAY"),
            tmux: std::env::var_os("TMUX"),
            sty: std::env::var_os("STY"),
            term: std::env::var_os("TERM"),
            path: std::env::var_os("PATH"),
        }
    }

    fn tmux_active(&self) -> bool {
        self.tmux.is_some()
    }

    fn screen_active(&self) -> bool {
        self.sty.is_some()
    }

    fn display_value(value: &Option<OsString>) -> String {
        value
            .as_ref()
            .map(|value| value.to_string_lossy().into_owned())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "<unset>".to_string())
    }
}

#[derive(Debug, Clone)]
struct ClipboardCommand {
    program: &'static str,
    args: Vec<&'static str>,
}

#[derive(Debug, Clone)]
struct HostWriteOutcome {
    transport: String,
    verified: bool,
    warning: Option<String>,
}

trait ClipboardBackend {
    fn command_exists(&self, program: &str, env: &ClipboardEnvironment) -> bool;
    fn write_command(&self, program: &str, args: &[&str], payload: &[u8]) -> Result<(), String>;
    fn read_command(&self, program: &str, args: &[&str]) -> Result<String, String>;
    fn write_osc52(&self, text: &str, env: &ClipboardEnvironment) -> Result<(), String>;
}

struct RealClipboardBackend;

impl ClipboardBackend for RealClipboardBackend {
    fn command_exists(&self, program: &str, env: &ClipboardEnvironment) -> bool {
        program_exists_in_path(program, env.path.as_deref())
    }

    fn write_command(&self, program: &str, args: &[&str], payload: &[u8]) -> Result<(), String> {
        run_clipboard_write(program, args, payload)
    }

    fn read_command(&self, program: &str, args: &[&str]) -> Result<String, String> {
        run_clipboard_read(program, args)
    }

    fn write_osc52(&self, text: &str, env: &ClipboardEnvironment) -> Result<(), String> {
        let mut tty = OpenOptions::new()
            .write(true)
            .open("/dev/tty")
            .map_err(|error| format!("open /dev/tty: {error}"))?;
        write_osc52_clipboard_to_with_multiplexer(
            &mut tty,
            text,
            env.tmux_active(),
            env.screen_active(),
        )
        .map_err(|error| format!("write /dev/tty: {error}"))?
        .then_some(())
        .ok_or_else(|| {
            format!(
                "payload exceeds the OSC 52 limit of {} bytes",
                OSC52_TEXT_CLIPBOARD_MAX_BYTES
            )
        })
    }
}

static HOST_CLIPBOARD_WRITE_STATE: OnceLock<Mutex<HostClipboardWriteState>> = OnceLock::new();
static HOST_CLIPBOARD_MESSAGE_TX: OnceLock<Mutex<Option<mpsc::Sender<AppMessage>>>> = OnceLock::new();
static HOST_CLIPBOARD_HISTORY: OnceLock<Mutex<VecDeque<ClipboardAttempt>>> = OnceLock::new();

fn write_state() -> &'static Mutex<HostClipboardWriteState> {
    HOST_CLIPBOARD_WRITE_STATE.get_or_init(|| Mutex::new(HostClipboardWriteState::default()))
}

fn message_sender() -> &'static Mutex<Option<mpsc::Sender<AppMessage>>> {
    HOST_CLIPBOARD_MESSAGE_TX.get_or_init(|| Mutex::new(None))
}

fn history() -> &'static Mutex<VecDeque<ClipboardAttempt>> {
    HOST_CLIPBOARD_HISTORY.get_or_init(|| Mutex::new(VecDeque::new()))
}

pub(crate) fn configure_message_sender(tx: mpsc::Sender<AppMessage>) {
    let mut slot = message_sender()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *slot = Some(tx);
}

fn deliver_app_message_nonblocking(
    tx: mpsc::Sender<AppMessage>,
    message: AppMessage,
    worker_name: &'static str,
) {
    match tx.try_send(message) {
        Ok(()) => {}
        Err(mpsc::error::TrySendError::Full(message)) => {
            let fallback = std::thread::Builder::new()
                .name(worker_name.to_string())
                .spawn(move || {
                    if let Err(error) = tx.blocking_send(message) {
                        log::warn!("clipboard message delivery failed: {error}");
                    }
                });
            if let Err(error) = fallback {
                log::warn!("clipboard message fallback could not start: {error}");
            }
        }
        Err(mpsc::error::TrySendError::Closed(_)) => {
            log::warn!("clipboard message delivery failed: application channel is closed");
        }
    }
}

fn send_status(message: String) {
    let tx = message_sender()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    if let Some(tx) = tx {
        deliver_app_message_nonblocking(
            tx,
            AppMessage::StatusMessage(message),
            "tonepoet-host-clipboard-status-delivery",
        );
    } else {
        log::warn!("{message}");
    }
}

fn record_attempt(
    operation: ClipboardOperation,
    transport: impl Into<String>,
    outcome: Result<String, String>,
) {
    let mut attempts = history()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    attempts.push_back(ClipboardAttempt {
        at: SystemTime::now(),
        operation,
        transport: transport.into(),
        outcome,
    });
    while attempts.len() > CLIPBOARD_HISTORY_LIMIT {
        attempts.pop_front();
    }
}

/// Publication hook installed into `tui-file-picker`.
///
/// The internal clipboard has already committed before this function runs.
/// Rapid host writes are coalesced using last-value-wins semantics.
pub(crate) fn publish_system_clipboard(text: &str) {
    let should_start = {
        let mut state = write_state()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.pending = Some(text.to_string());
        if state.worker_running {
            false
        } else {
            state.worker_running = true;
            true
        }
    };

    if should_start {
        let spawn_result = std::thread::Builder::new()
            .name("tonepoet-host-clipboard-write".to_string())
            .spawn(host_clipboard_write_worker);
        if let Err(error) = spawn_result {
            let mut state = write_state()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.worker_running = false;
            state.pending = None;
            let detail = format!("could not start host clipboard worker: {error}");
            record_attempt(ClipboardOperation::Write, "worker", Err(detail.clone()));
            send_status(format!(
                "Copied internally; host clipboard unavailable: {detail}"
            ));
        }
    }
}

fn host_clipboard_write_worker() {
    let backend = RealClipboardBackend;
    host_clipboard_write_worker_with(
        &backend,
        write_state(),
        ClipboardEnvironment::detect,
        send_status,
    );
}

fn host_clipboard_write_worker_with<B, E, S>(
    backend: &B,
    state: &Mutex<HostClipboardWriteState>,
    mut detect_environment: E,
    mut report_status: S,
) where
    B: ClipboardBackend,
    E: FnMut() -> ClipboardEnvironment,
    S: FnMut(String),
{
    loop {
        let next = {
            let mut state = state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            match state.pending.take() {
                Some(text) => text,
                None => {
                    state.worker_running = false;
                    return;
                }
            }
        };

        let env = detect_environment();
        match write_host_clipboard_with(backend, &env, &next, ClipboardOperation::Write) {
            Ok(outcome) => {
                if let Some(warning) = outcome.warning {
                    report_status(format!("Copied internally; {warning}"));
                }
            }
            Err(error) => {
                log::debug!("host clipboard write unavailable: {error}");
                report_status(format!(
                    "Copied internally; host clipboard unavailable: {error}; run :clipboard"
                ));
            }
        }
    }
}

/// Launch a bounded host clipboard read. The generation and semantic target
/// prevent a late completion from mutating a different editor.
pub(crate) fn request_host_clipboard_paste(
    tx: mpsc::Sender<AppMessage>,
    generation: u64,
    target: HostClipboardPasteTarget,
) {
    let fallback_tx = tx.clone();
    let fallback_target = target.clone();
    let spawn_result = std::thread::Builder::new()
        .name("tonepoet-host-clipboard-read".to_string())
        .spawn(move || {
            let env = ClipboardEnvironment::detect();
            let backend = RealClipboardBackend;
            let result = read_host_clipboard_with(&backend, &env, ClipboardOperation::Read);
            let _ = tx.blocking_send(AppMessage::HostClipboardReadComplete {
                generation,
                target,
                result,
            });
        });

    if let Err(error) = spawn_result {
        let detail = format!("could not start host clipboard reader: {error}");
        record_attempt(ClipboardOperation::Read, "worker", Err(detail.clone()));
        deliver_app_message_nonblocking(
            fallback_tx,
            AppMessage::HostClipboardReadComplete {
                generation,
                target: fallback_target,
                result: Err(detail),
            },
            "tonepoet-host-clipboard-read-delivery",
        );
    }
}

pub(crate) fn request_clipboard_diagnostics(tx: mpsc::Sender<AppMessage>) {
    let fallback_tx = tx.clone();
    let spawn_result = std::thread::Builder::new()
        .name("tonepoet-host-clipboard-diagnostic".to_string())
        .spawn(move || {
            let report = clipboard_diagnostic_report(&RealClipboardBackend);
            let _ = tx.blocking_send(AppMessage::HostClipboardDiagnosticComplete { report });
        });
    if let Err(error) = spawn_result {
        deliver_app_message_nonblocking(
            fallback_tx,
            AppMessage::HostClipboardDiagnosticComplete {
                report: format!("Clipboard diagnostic could not start: {error}"),
            },
            "tonepoet-host-clipboard-diagnostic-delivery",
        );
    }
}

fn clipboard_diagnostic_report(backend: &impl ClipboardBackend) -> String {
    let env = ClipboardEnvironment::detect();
    let write_candidates = native_write_candidates(backend, &env);
    let read_candidates = native_read_candidates(backend, &env);
    let original = read_host_clipboard_with(backend, &env, ClipboardOperation::Diagnostic);
    let self_test = match original {
        Ok(original) => {
            let nonce = format!(
                "tonepoet-clipboard-self-test-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            );
            let write_result = write_host_clipboard_with(
                backend,
                &env,
                &nonce,
                ClipboardOperation::Diagnostic,
            );
            let readback = read_host_clipboard_with(
                backend,
                &env,
                ClipboardOperation::Diagnostic,
            );
            let result = match (&write_result, &readback) {
                (Ok(write), Ok(value)) if value == &nonce => format!(
                    "PASS via {} (write/read round-trip matched)",
                    write.transport
                ),
                (Ok(write), Ok(_)) => format!(
                    "FAIL via {} (read-back did not match the test payload)",
                    write.transport
                ),
                (Ok(write), Err(error)) if !write.verified => format!(
                    "WRITE-ONLY via {} ({error}); terminal OSC 52 acceptance cannot be read back",
                    write.transport
                ),
                (Ok(write), Err(error)) => {
                    format!("FAIL after {} write: {error}", write.transport)
                }
                (Err(error), _) => format!("FAIL: {error}"),
            };
            let restore = write_host_clipboard_with(
                backend,
                &env,
                &original,
                ClipboardOperation::Diagnostic,
            );
            match restore {
                Ok(_) => result,
                Err(error) => format!(
                    "{result}; WARNING: failed to restore the prior clipboard: {error}"
                ),
            }
        }
        Err(error) => format!(
            "SKIPPED destructive write test because the prior clipboard could not be read and restored: {error}"
        ),
    };

    let write_names = if write_candidates.is_empty() {
        "<none>".to_string()
    } else {
        write_candidates
            .iter()
            .map(|candidate| candidate.program)
            .collect::<Vec<_>>()
            .join(", ")
    };
    let read_names = if read_candidates.is_empty() {
        "<none>".to_string()
    } else {
        read_candidates
            .iter()
            .map(|candidate| candidate.program)
            .collect::<Vec<_>>()
            .join(", ")
    };

    let attempts = history()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .iter()
        .rev()
        .take(12)
        .cloned()
        .collect::<Vec<_>>();
    let mut report = format!(
        "Environment\n  WAYLAND_DISPLAY={}\n  DISPLAY={}\n  TMUX={}\n  STY={}\n  TERM={}\n\nDetected transports\n  write: {}{}\n  read: {}\n\nLive self-test\n  {}\n\nRecent attempts",
        ClipboardEnvironment::display_value(&env.wayland_display),
        ClipboardEnvironment::display_value(&env.display),
        ClipboardEnvironment::display_value(&env.tmux),
        ClipboardEnvironment::display_value(&env.sty),
        ClipboardEnvironment::display_value(&env.term),
        write_names,
        if OSC52_TEXT_CLIPBOARD_MAX_BYTES > 0 {
            ", OSC52(/dev/tty)"
        } else {
            ""
        },
        read_names,
        self_test,
    );
    if attempts.is_empty() {
        report.push_str("\n  <none>");
    } else {
        for attempt in attempts.into_iter().rev() {
            let timestamp = attempt
                .at
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let result = match attempt.outcome {
                Ok(detail) => format!("ok: {detail}"),
                Err(detail) => format!("error: {detail}"),
            };
            report.push_str(&format!(
                "\n  {timestamp} {} {} — {}",
                attempt.operation.label(),
                attempt.transport,
                result
            ));
        }
    }
    report
}

fn write_host_clipboard_with(
    backend: &impl ClipboardBackend,
    env: &ClipboardEnvironment,
    text: &str,
    operation: ClipboardOperation,
) -> Result<HostWriteOutcome, String> {
    let mut errors = Vec::new();
    if text.len() <= NATIVE_CLIPBOARD_MAX_BYTES {
        for candidate in native_write_candidates(backend, env) {
            match backend.write_command(candidate.program, &candidate.args, text.as_bytes()) {
                Ok(()) => {
                    record_attempt(
                        operation,
                        candidate.program,
                        Ok(format!("{} bytes", text.len())),
                    );
                    return Ok(HostWriteOutcome {
                        transport: candidate.program.to_string(),
                        verified: true,
                        warning: None,
                    });
                }
                Err(error) => {
                    record_attempt(operation, candidate.program, Err(error.clone()));
                    errors.push(format!("{}: {error}", candidate.program));
                }
            }
        }
    } else {
        errors.push(format!(
            "native payload exceeds {} bytes",
            NATIVE_CLIPBOARD_MAX_BYTES
        ));
    }

    match backend.write_osc52(text, env) {
        Ok(()) => {
            record_attempt(
                operation,
                "OSC52",
                Ok(format!("{} bytes written to /dev/tty", text.len())),
            );
            let warning = if env.tmux_active() {
                Some(
                    "host mirror sent through OSC 52; tmux must permit clipboard passthrough (`set-clipboard on`)"
                        .to_string(),
                )
            } else if env.screen_active() {
                Some(
                    "host mirror sent through OSC 52; screen/byobu terminal acceptance is not verifiable"
                        .to_string(),
                )
            } else {
                Some(
                    "host mirror sent through OSC 52; terminal acceptance is not verifiable"
                        .to_string(),
                )
            };
            Ok(HostWriteOutcome {
                transport: "OSC52".to_string(),
                verified: false,
                warning,
            })
        }
        Err(error) => {
            record_attempt(operation, "OSC52", Err(error.clone()));
            errors.push(format!("OSC52: {error}"));
            Err(actionable_write_error(env, &errors))
        }
    }
}

fn read_host_clipboard_with(
    backend: &impl ClipboardBackend,
    env: &ClipboardEnvironment,
    operation: ClipboardOperation,
) -> Result<String, String> {
    let candidates = native_read_candidates(backend, env);
    if candidates.is_empty() {
        let detail = actionable_read_error(env, &[]);
        record_attempt(operation, "native-read", Err(detail.clone()));
        return Err(detail);
    }

    let mut errors = Vec::new();
    for candidate in candidates {
        match backend.read_command(candidate.program, &candidate.args) {
            Ok(text) => {
                record_attempt(
                    operation,
                    candidate.program,
                    Ok(format!("{} bytes", text.len())),
                );
                return Ok(text);
            }
            Err(error) => {
                record_attempt(operation, candidate.program, Err(error.clone()));
                errors.push(format!("{}: {error}", candidate.program));
            }
        }
    }
    Err(actionable_read_error(env, &errors))
}

fn native_write_candidates(
    backend: &impl ClipboardBackend,
    env: &ClipboardEnvironment,
) -> Vec<ClipboardCommand> {
    let mut candidates = Vec::new();
    if env.wayland_display.is_some() && backend.command_exists("wl-copy", env) {
        candidates.push(ClipboardCommand {
            program: "wl-copy",
            args: vec!["--type", "text/plain;charset=utf-8"],
        });
    }
    if env.display.is_some() {
        if backend.command_exists("xclip", env) {
            candidates.push(ClipboardCommand {
                program: "xclip",
                args: vec!["-selection", "clipboard", "-in"],
            });
        }
        if backend.command_exists("xsel", env) {
            candidates.push(ClipboardCommand {
                program: "xsel",
                args: vec!["--clipboard", "--input"],
            });
        }
    }
    candidates
}

fn native_read_candidates(
    backend: &impl ClipboardBackend,
    env: &ClipboardEnvironment,
) -> Vec<ClipboardCommand> {
    let mut candidates = Vec::new();
    if env.wayland_display.is_some() && backend.command_exists("wl-paste", env) {
        candidates.push(ClipboardCommand {
            program: "wl-paste",
            args: vec!["--no-newline", "--type", "text"],
        });
    }
    if env.display.is_some() {
        if backend.command_exists("xclip", env) {
            candidates.push(ClipboardCommand {
                program: "xclip",
                args: vec!["-selection", "clipboard", "-out"],
            });
        }
        if backend.command_exists("xsel", env) {
            candidates.push(ClipboardCommand {
                program: "xsel",
                args: vec!["--clipboard", "--output"],
            });
        }
    }
    candidates
}

fn actionable_write_error(env: &ClipboardEnvironment, errors: &[String]) -> String {
    let mut reason = if env.wayland_display.is_none() && env.display.is_none() {
        "no WAYLAND_DISPLAY or DISPLAY; native clipboard helpers were not eligible".to_string()
    } else {
        "no usable wl-copy/xclip/xsel transport".to_string()
    };
    if !errors.is_empty() {
        reason.push_str(&format!(" ({})", errors.join("; ")));
    }
    if env.tmux_active() {
        reason.push_str("; OSC 52 through tmux also failed (check `set-clipboard on` and passthrough)");
    } else if env.screen_active() {
        reason.push_str("; OSC 52 through screen/byobu also failed");
    } else {
        reason.push_str("; OSC 52 to /dev/tty also failed");
    }
    reason
}

fn actionable_read_error(env: &ClipboardEnvironment, errors: &[String]) -> String {
    let mut reason = if env.wayland_display.is_none() && env.display.is_none() {
        "host clipboard read requires WAYLAND_DISPLAY or DISPLAY".to_string()
    } else {
        "install wl-clipboard, xclip, or xsel, or fix the detected helper".to_string()
    };
    if !errors.is_empty() {
        reason.push_str(&format!(" ({})", errors.join("; ")));
    }
    reason
}

fn program_exists_in_path(program: &str, path: Option<&std::ffi::OsStr>) -> bool {
    let Some(path) = path else {
        return false;
    };
    std::env::split_paths(path).any(|directory| is_executable_file(&directory.join(program)))
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
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

fn run_clipboard_write(program: &str, args: &[&str], payload: &[u8]) -> Result<(), String> {
    // Clipboard writers such as xclip may fork a long-lived selection owner.
    // Piping stderr and waiting for EOF would then wait on every descendant
    // that inherited the descriptor, defeating the command timeout and
    // stranding the coalescing worker. Native write failures remain actionable
    // through the helper name and exit status; foreground reads still retain
    // bounded stdout/stderr capture below.
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| error.to_string())?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| format!("{program} did not provide stdin"))?;
    let payload = payload.to_vec();
    let writer = std::thread::spawn(move || stdin.write_all(&payload));
    let status = wait_for_child_with_timeout(&mut child, program);
    let write_result = writer
        .join()
        .map_err(|_| format!("{program} clipboard writer panicked"))?;
    write_result.map_err(|error| error.to_string())?;
    let status = status?;

    if status.success() {
        Ok(())
    } else {
        Err(format!("{program} exited with {status}"))
    }
}

fn run_clipboard_read(program: &str, args: &[&str]) -> Result<String, String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| error.to_string())?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| format!("{program} did not provide stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| format!("{program} did not provide stderr"))?;
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout
            .take((NATIVE_CLIPBOARD_MAX_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
    });
    let stderr_reader = std::thread::spawn(move || read_bounded_stderr(stderr));
    let status = wait_for_child_with_timeout(&mut child, program);
    let bytes = reader
        .join()
        .map_err(|_| format!("{program} clipboard reader panicked"))?
        .map_err(|error| error.to_string())?;
    let stderr = stderr_reader
        .join()
        .map_err(|_| format!("{program} stderr reader panicked"))??;
    let status = status?;

    if !status.success() {
        return if stderr.trim().is_empty() {
            Err(format!("{program} exited with {status}"))
        } else {
            Err(format!("{program} exited with {status}: {}", stderr.trim()))
        };
    }
    if bytes.len() > NATIVE_CLIPBOARD_MAX_BYTES {
        return Err(format!(
            "clipboard payload exceeds {} bytes",
            NATIVE_CLIPBOARD_MAX_BYTES
        ));
    }
    String::from_utf8(bytes).map_err(|_| "clipboard text is not valid UTF-8".to_string())
}

fn read_bounded_stderr(stderr: impl Read) -> Result<String, String> {
    let mut bytes = Vec::new();
    stderr
        .take(8 * 1024)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn wait_for_child_with_timeout(
    child: &mut std::process::Child,
    program: &str,
) -> Result<std::process::ExitStatus, String> {
    let deadline = Instant::now() + CLIPBOARD_COMMAND_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(CLIPBOARD_POLL_INTERVAL);
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{program} timed out after {} ms",
                    CLIPBOARD_COMMAND_TIMEOUT.as_millis()
                ));
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error.to_string());
            }
        }
    }
}

pub(crate) fn write_osc52_clipboard_to_with_multiplexer(
    writer: &mut impl Write,
    text: &str,
    tmux_passthrough: bool,
    screen_passthrough: bool,
) -> std::io::Result<bool> {
    if text.len() > OSC52_TEXT_CLIPBOARD_MAX_BYTES {
        return Ok(false);
    }

    let osc = format!("\x1b]52;c;{}\x07", base64_encode(text.as_bytes()));
    if tmux_passthrough {
        writer.write_all(b"\x1bPtmux;")?;
        for byte in osc.bytes() {
            if byte == 0x1b {
                writer.write_all(b"\x1b\x1b")?;
            } else {
                writer.write_all(&[byte])?;
            }
        }
        writer.write_all(b"\x1b\\")?;
    } else if screen_passthrough {
        writer.write_all(b"\x1bP")?;
        writer.write_all(osc.as_bytes())?;
        writer.write_all(b"\x1b\\")?;
    } else {
        writer.write_all(osc.as_bytes())?;
    }
    writer.flush()?;
    Ok(true)
}

fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(CHARS[((n >> 18) & 0x3f) as usize] as char);
        out.push(CHARS[((n >> 12) & 0x3f) as usize] as char);
        out.push(if chunk.len() > 1 {
            CHARS[((n >> 6) & 0x3f) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            CHARS[(n & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

    struct FakeBackend {
        programs: BTreeSet<String>,
        writes: Mutex<Vec<String>>,
        write_results: BTreeMap<String, Result<(), String>>,
        read_results: BTreeMap<String, Result<String, String>>,
        osc_result: Result<(), String>,
    }

    impl Default for FakeBackend {
        fn default() -> Self {
            Self {
                programs: BTreeSet::new(),
                writes: Mutex::new(Vec::new()),
                write_results: BTreeMap::new(),
                read_results: BTreeMap::new(),
                osc_result: Ok(()),
            }
        }
    }

    impl ClipboardBackend for FakeBackend {
        fn command_exists(&self, program: &str, _env: &ClipboardEnvironment) -> bool {
            self.programs.contains(program)
        }

        fn write_command(
            &self,
            program: &str,
            _args: &[&str],
            _payload: &[u8],
        ) -> Result<(), String> {
            self.writes
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(program.to_string());
            self.write_results
                .get(program)
                .cloned()
                .unwrap_or(Ok(()))
        }

        fn read_command(&self, program: &str, _args: &[&str]) -> Result<String, String> {
            self.read_results
                .get(program)
                .cloned()
                .unwrap_or_else(|| Err("no fixture".to_string()))
        }

        fn write_osc52(&self, _text: &str, _env: &ClipboardEnvironment) -> Result<(), String> {
            self.osc_result.clone()
        }
    }

    fn x11_env() -> ClipboardEnvironment {
        ClipboardEnvironment {
            wayland_display: None,
            display: Some(OsString::from(":0")),
            tmux: None,
            sty: None,
            term: Some(OsString::from("xterm-256color")),
            path: None,
        }
    }

    #[cfg(unix)]
    struct ForkingWriteBackend {
        log_path: std::path::PathBuf,
        first_started: Mutex<Option<std::sync::mpsc::Sender<()>>>,
        calls: std::sync::atomic::AtomicUsize,
    }

    #[cfg(unix)]
    impl ClipboardBackend for ForkingWriteBackend {
        fn command_exists(&self, program: &str, _env: &ClipboardEnvironment) -> bool {
            program == "xclip"
        }

        fn write_command(
            &self,
            program: &str,
            _args: &[&str],
            payload: &[u8],
        ) -> Result<(), String> {
            assert_eq!(program, "xclip");
            if self
                .calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                == 0
            {
                if let Some(sender) = self
                    .first_started
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .take()
                {
                    let _ = sender.send(());
                }
            }

            let log_path = self.log_path.to_string_lossy();
            let args = [
                "-c",
                "value=$(cat); printf '%s\\n' \"$value\" >> \"$1\"; sleep 5 &",
                "tonepoet-clipboard-test",
                log_path.as_ref(),
            ];
            run_clipboard_write("sh", &args, payload)
        }

        fn read_command(&self, _program: &str, _args: &[&str]) -> Result<String, String> {
            Err("unused".to_string())
        }

        fn write_osc52(
            &self,
            _text: &str,
            _env: &ClipboardEnvironment,
        ) -> Result<(), String> {
            Err("unused".to_string())
        }
    }

    #[cfg(unix)]
    #[test]
    fn native_write_does_not_wait_for_a_descendant_that_inherits_stderr() {
        let started = Instant::now();
        run_clipboard_write("sh", &["-c", "cat >/dev/null; sleep 5 &"], b"Duke")
            .expect("immediate clipboard owner parent exits successfully");
        assert!(
            started.elapsed() < CLIPBOARD_COMMAND_TIMEOUT,
            "native write waited for a background descendant instead of the immediate child: {:?}",
            started.elapsed(),
        );
    }

    #[cfg(unix)]
    #[test]
    fn write_worker_drains_pending_after_a_forking_clipboard_helper() {
        let temp = tempfile::tempdir().expect("tempdir");
        let log_path = temp.path().join("writes.log");
        let (first_started_tx, first_started_rx) = std::sync::mpsc::channel();
        let backend = std::sync::Arc::new(ForkingWriteBackend {
            log_path: log_path.clone(),
            first_started: Mutex::new(Some(first_started_tx)),
            calls: std::sync::atomic::AtomicUsize::new(0),
        });
        let state = std::sync::Arc::new(Mutex::new(HostClipboardWriteState {
            pending: Some("first".to_string()),
            worker_running: true,
        }));

        let worker_backend = std::sync::Arc::clone(&backend);
        let worker_state = std::sync::Arc::clone(&state);
        let started = Instant::now();
        let worker = std::thread::spawn(move || {
            host_clipboard_write_worker_with(
                worker_backend.as_ref(),
                worker_state.as_ref(),
                x11_env,
                |_| {},
            );
        });

        first_started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("first helper invocation");
        {
            let mut state = state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            assert!(state.worker_running);
            state.pending = Some("second".to_string());
        }

        worker.join().expect("clipboard worker");
        assert!(
            started.elapsed() < CLIPBOARD_COMMAND_TIMEOUT + Duration::from_secs(1),
            "worker remained stranded behind a descendant-owned pipe: {:?}",
            started.elapsed(),
        );
        let writes = std::fs::read_to_string(&log_path).expect("helper write log");
        assert_eq!(writes.lines().collect::<Vec<_>>(), vec!["first", "second"]);
        assert_eq!(
            backend.calls.load(std::sync::atomic::Ordering::SeqCst),
            2,
        );
        let state = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert!(!state.worker_running);
        assert!(state.pending.is_none());
    }

    #[test]
    fn xsel_only_is_selected_without_attempting_missing_xclip() {
        let mut backend = FakeBackend::default();
        backend.programs.insert("xsel".to_string());
        backend.osc_result = Err("unused".to_string());
        let outcome = write_host_clipboard_with(
            &backend,
            &x11_env(),
            "Duke",
            ClipboardOperation::Diagnostic,
        )
        .expect("xsel write");
        assert_eq!(outcome.transport, "xsel");
        assert_eq!(
            backend
                .writes
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .as_slice(),
            &["xsel".to_string()]
        );
    }

    #[test]
    fn xsel_only_read_uses_the_clipboard_selection() {
        let mut backend = FakeBackend::default();
        backend.programs.insert("xsel".to_string());
        backend
            .read_results
            .insert("xsel".to_string(), Ok("Duke".to_string()));

        let value = read_host_clipboard_with(
            &backend,
            &x11_env(),
            ClipboardOperation::Diagnostic,
        )
        .expect("xsel read");
        assert_eq!(value, "Duke");
    }

    #[test]
    fn no_native_tools_falls_back_to_osc52() {
        let backend = FakeBackend::default();
        let outcome = write_host_clipboard_with(
            &backend,
            &x11_env(),
            "Duke",
            ClipboardOperation::Diagnostic,
        )
        .expect("OSC52 fallback");
        assert_eq!(outcome.transport, "OSC52");
        assert!(!outcome.verified);
    }

    #[test]
    fn failed_native_and_osc52_write_produces_actionable_error() {
        let mut backend = FakeBackend::default();
        backend.programs.insert("xsel".to_string());
        backend
            .write_results
            .insert("xsel".to_string(), Err("selection owner refused".to_string()));
        backend.osc_result = Err("no tty".to_string());
        let error = write_host_clipboard_with(
            &backend,
            &x11_env(),
            "Duke",
            ClipboardOperation::Diagnostic,
        )
        .expect_err("write must fail");
        assert!(error.contains("xsel"));
        assert!(error.contains("OSC 52"));
    }

    #[test]
    fn osc52_encoding_is_exact_for_plain_tmux_and_screen_paths() {
        let mut plain = Vec::new();
        assert!(write_osc52_clipboard_to_with_multiplexer(
            &mut plain, "Duke", false, false,
        )
        .expect("plain OSC 52"));
        assert_eq!(plain, b"\x1b]52;c;RHVrZQ==\x07");

        let mut tmux = Vec::new();
        assert!(write_osc52_clipboard_to_with_multiplexer(
            &mut tmux, "Duke", true, false,
        )
        .expect("tmux OSC 52"));
        assert_eq!(tmux, b"\x1bPtmux;\x1b\x1b]52;c;RHVrZQ==\x07\x1b\\");

        let mut screen = Vec::new();
        assert!(write_osc52_clipboard_to_with_multiplexer(
            &mut screen, "Duke", false, true,
        )
        .expect("screen OSC 52"));
        assert_eq!(screen, b"\x1bP\x1b]52;c;RHVrZQ==\x07\x1b\\");
    }

    #[test]
    fn osc52_refuses_oversized_payloads_without_partial_output() {
        let mut output = Vec::new();
        assert!(!write_osc52_clipboard_to_with_multiplexer(
            &mut output,
            &"x".repeat(OSC52_TEXT_CLIPBOARD_MAX_BYTES + 1),
            true,
            false,
        )
        .expect("oversized OSC 52"));
        assert!(output.is_empty());
    }
}
