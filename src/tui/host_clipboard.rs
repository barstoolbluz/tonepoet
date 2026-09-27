//! Host clipboard integration with bounded workers, observable outcomes, and diagnostics.
//!
//! Tonepoet's terminal/host clipboard is the user-visible clipboard authority.
//! Writes are asynchronous and coalesced, every external action has a two-second
//! deadline, and failures are surfaced rather than being hidden behind a stale
//! in-process fallback.

use std::collections::VecDeque;
use std::ffi::OsString;
#[cfg(target_os = "linux")]
use std::fs::File;
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
const CLIPBOARD_WRITE_DRAIN_TIMEOUT: Duration = Duration::from_secs(8);
const CLIPBOARD_HISTORY_LIMIT: usize = 32;
#[cfg(target_os = "linux")]
const LINUX_ANCESTOR_SCAN_LIMIT: usize = 64;
#[cfg(target_os = "linux")]
const LINUX_PROC_ENV_MAX_BYTES: usize = 256 * 1024;

#[derive(Default)]
struct HostClipboardWriteState {
    pending: Option<String>,
    worker_running: bool,
    last_error: Option<String>,
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

#[derive(Debug, Clone, Default)]
struct ClipboardEnvironment {
    wayland_display: Option<OsString>,
    display: Option<OsString>,
    xdg_runtime_dir: Option<OsString>,
    xauthority: Option<OsString>,
    tmux: Option<OsString>,
    sty: Option<OsString>,
    term: Option<OsString>,
    path: Option<OsString>,
    wayland_display_source: Option<String>,
    display_source: Option<String>,
    remote_session: bool,
}

impl ClipboardEnvironment {
    fn detect() -> Self {
        let wayland_display = nonempty_env_var("WAYLAND_DISPLAY");
        let display = nonempty_env_var("DISPLAY");
        let env = Self {
            wayland_display_source: wayland_display
                .as_ref()
                .map(|_| "process environment".to_string()),
            display_source: display
                .as_ref()
                .map(|_| "process environment".to_string()),
            wayland_display,
            display,
            xdg_runtime_dir: nonempty_env_var("XDG_RUNTIME_DIR"),
            xauthority: nonempty_env_var("XAUTHORITY"),
            tmux: nonempty_env_var("TMUX"),
            sty: nonempty_env_var("STY"),
            term: nonempty_env_var("TERM"),
            path: nonempty_env_var("PATH"),
            remote_session: nonempty_env_var("SSH_CONNECTION").is_some()
                || nonempty_env_var("SSH_TTY").is_some(),
        };
        #[cfg(target_os = "linux")]
        {
            let mut env = env;
            recover_linux_graphical_environment(&mut env);
            env
        }
        #[cfg(not(target_os = "linux"))]
        {
            env
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

    fn display_value_with_source(value: &Option<OsString>, source: &Option<String>) -> String {
        let value = Self::display_value(value);
        match source {
            Some(source) if value != "<unset>" => format!("{value} ({source})"),
            _ => value,
        }
    }
}

fn nonempty_env_var(name: &str) -> Option<OsString> {
    std::env::var_os(name).filter(|value| !value.is_empty())
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Default)]
struct LinuxAncestorEnvironment {
    wayland_display: Option<OsString>,
    display: Option<OsString>,
    xdg_runtime_dir: Option<OsString>,
    xauthority: Option<OsString>,
    remote_session: bool,
}

#[cfg(target_os = "linux")]
fn recover_linux_graphical_environment(env: &mut ClipboardEnvironment) {
    if env.display.is_some()
        || (env.wayland_display.is_some() && env.xdg_runtime_dir.is_some())
    {
        return;
    }

    let Some((mut pid, current_euid)) = linux_process_parent_and_euid(std::process::id()) else {
        infer_wayland_display_from_runtime(env);
        return;
    };

    for _ in 0..LINUX_ANCESTOR_SCAN_LIMIT {
        if pid <= 1 {
            break;
        }
        let Some((parent_pid, ancestor_euid)) = linux_process_parent_and_euid(pid) else {
            break;
        };
        if ancestor_euid == current_euid {
            if let Some(ancestor) = read_linux_ancestor_environment(pid) {
                merge_linux_ancestor_environment(env, pid, &ancestor);
            }
        }
        if parent_pid == 0 || parent_pid == pid {
            break;
        }
        pid = parent_pid;
    }

    infer_wayland_display_from_runtime(env);
}

#[cfg(target_os = "linux")]
fn infer_wayland_display_from_runtime(env: &mut ClipboardEnvironment) {
    if env.wayland_display.is_some() || env.remote_session {
        return;
    }
    let Some(runtime_dir) = env.xdg_runtime_dir.as_deref() else {
        return;
    };
    let Some(display) = discover_unique_wayland_socket(Path::new(runtime_dir)) else {
        return;
    };
    env.wayland_display = Some(display.clone());
    env.wayland_display_source = Some(format!(
        "unique socket {}",
        Path::new(runtime_dir).join(Path::new(&display)).display()
    ));
}

#[cfg(target_os = "linux")]
fn linux_process_parent_and_euid(pid: u32) -> Option<(u32, u32)> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let mut parent_pid = None;
    let mut effective_uid = None;
    for line in status.lines() {
        if let Some(value) = line.strip_prefix("PPid:") {
            parent_pid = value.trim().parse::<u32>().ok();
        } else if let Some(value) = line.strip_prefix("Uid:") {
            effective_uid = value
                .split_whitespace()
                .nth(1)
                .and_then(|value| value.parse::<u32>().ok());
        }
        if parent_pid.is_some() && effective_uid.is_some() {
            break;
        }
    }
    Some((parent_pid?, effective_uid?))
}

#[cfg(target_os = "linux")]
fn read_linux_ancestor_environment(pid: u32) -> Option<LinuxAncestorEnvironment> {
    let file = File::open(format!("/proc/{pid}/environ")).ok()?;
    let mut bytes = Vec::new();
    file.take((LINUX_PROC_ENV_MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > LINUX_PROC_ENV_MAX_BYTES {
        return None;
    }
    Some(parse_linux_ancestor_environment(&bytes))
}

#[cfg(target_os = "linux")]
fn parse_linux_ancestor_environment(bytes: &[u8]) -> LinuxAncestorEnvironment {
    use std::os::unix::ffi::OsStringExt;

    let mut result = LinuxAncestorEnvironment::default();
    for entry in bytes.split(|byte| *byte == 0).filter(|entry| !entry.is_empty()) {
        let Some(separator) = entry.iter().position(|byte| *byte == b'=') else {
            continue;
        };
        let key = &entry[..separator];
        let value = &entry[separator + 1..];
        if value.is_empty() {
            continue;
        }
        let value = || OsString::from_vec(value.to_vec());
        if key == b"WAYLAND_DISPLAY" {
            result.wayland_display = Some(value());
        } else if key == b"DISPLAY" {
            result.display = Some(value());
        } else if key == b"XDG_RUNTIME_DIR" {
            result.xdg_runtime_dir = Some(value());
        } else if key == b"XAUTHORITY" {
            result.xauthority = Some(value());
        } else if key == b"SSH_CONNECTION" || key == b"SSH_TTY" {
            result.remote_session = true;
        }
    }
    result
}

#[cfg(target_os = "linux")]
fn merge_linux_ancestor_environment(
    env: &mut ClipboardEnvironment,
    pid: u32,
    ancestor: &LinuxAncestorEnvironment,
) {
    env.remote_session |= ancestor.remote_session;

    if env.wayland_display.is_none() {
        if let Some(display) = ancestor.wayland_display.as_ref() {
            env.wayland_display = Some(display.clone());
            env.wayland_display_source = Some(format!("same-UID ancestor pid {pid}"));
            if ancestor.xdg_runtime_dir.is_some() {
                env.xdg_runtime_dir = ancestor.xdg_runtime_dir.clone();
            }
        }
    } else if env.xdg_runtime_dir.is_none()
        && ancestor.wayland_display.as_ref() == env.wayland_display.as_ref()
    {
        env.xdg_runtime_dir = ancestor.xdg_runtime_dir.clone();
    }

    if env.display.is_none() {
        if let Some(display) = ancestor.display.as_ref() {
            env.display = Some(display.clone());
            env.display_source = Some(format!("same-UID ancestor pid {pid}"));
            if ancestor.xauthority.is_some() {
                env.xauthority = ancestor.xauthority.clone();
            }
        }
    } else if env.xauthority.is_none() && ancestor.display.as_ref() == env.display.as_ref() {
        env.xauthority = ancestor.xauthority.clone();
    }

    if env.xdg_runtime_dir.is_none() {
        env.xdg_runtime_dir = ancestor.xdg_runtime_dir.clone();
    }
}

#[cfg(target_os = "linux")]
fn discover_unique_wayland_socket(runtime_dir: &Path) -> Option<OsString> {
    use std::os::unix::fs::FileTypeExt;

    let mut candidate = None;
    for entry in std::fs::read_dir(runtime_dir).ok()?.flatten() {
        let name = entry.file_name();
        let name_text = name.to_string_lossy();
        if !name_text.starts_with("wayland-") || name_text.ends_with(".lock") {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_socket() {
            continue;
        }
        if candidate.is_some() {
            return None;
        }
        candidate = Some(name);
    }
    candidate
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
    fn write_command(
        &self,
        program: &str,
        args: &[&str],
        payload: &[u8],
        env: &ClipboardEnvironment,
    ) -> Result<(), String>;
    fn read_command(
        &self,
        program: &str,
        args: &[&str],
        env: &ClipboardEnvironment,
    ) -> Result<String, String>;
    fn write_osc52(&self, text: &str, env: &ClipboardEnvironment) -> Result<(), String>;
}

struct RealClipboardBackend;

impl ClipboardBackend for RealClipboardBackend {
    fn command_exists(&self, program: &str, env: &ClipboardEnvironment) -> bool {
        program_exists_in_path(program, env.path.as_deref())
    }

    fn write_command(
        &self,
        program: &str,
        args: &[&str],
        payload: &[u8],
        env: &ClipboardEnvironment,
    ) -> Result<(), String> {
        run_clipboard_write(program, args, payload, env)
    }

    fn read_command(
        &self,
        program: &str,
        args: &[&str],
        env: &ClipboardEnvironment,
    ) -> Result<String, String> {
        run_clipboard_read(program, args, env)
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
/// Queue one host/terminal clipboard publication without blocking the reducer.
/// Rapid host writes are coalesced using last-value-wins semantics.
pub(crate) fn publish_system_clipboard(text: &str) {
    let should_start = {
        let mut state = write_state()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.pending = Some(text.to_string());
        state.last_error = None;
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
            state.last_error = Some(detail.clone());
            record_attempt(ClipboardOperation::Write, "worker", Err(detail.clone()));
            send_status(format!(
                "Tonepoet clipboard retained; external publication unavailable: {detail}"
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
                state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .last_error = None;
                if let Some(warning) = outcome.warning {
                    report_status(format!("External clipboard publication note: {warning}"));
                }
            }
            Err(error) => {
                state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .last_error = Some(error.clone());
                log::debug!("host clipboard write unavailable: {error}");
                report_status(format!(
                    "Tonepoet clipboard retained; external publication unavailable: {error}; run :clipboard"
                ));
            }
        }
    }
}

/// Wait off-thread for Tonepoet clipboard publications submitted before a
/// paste request to finish. Without this ordering point, an immediate Copy ->
/// Paste can race the asynchronous writer and read the host's previous value.
/// A bounded failure is safer than silently pasting stale external contents if
/// a host helper wedges despite its own command deadline.
fn wait_for_host_clipboard_write_state(
    state: &Mutex<HostClipboardWriteState>,
    timeout: Duration,
) -> Result<(), String> {
    let started = Instant::now();
    loop {
        let drained = {
            let state = state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            !state.worker_running && state.pending.is_none()
        };
        if drained {
            return Ok(());
        }
        if started.elapsed() >= timeout {
            return Err(
                "timed out waiting for a prior Tonepoet host-clipboard write to finish"
                    .to_string(),
            );
        }
        std::thread::sleep(CLIPBOARD_POLL_INTERVAL);
    }
}

#[cfg(test)]
fn wait_for_host_clipboard_write_completion(
    state: &Mutex<HostClipboardWriteState>,
    timeout: Duration,
) -> Result<(), String> {
    wait_for_host_clipboard_write_state(state, timeout)?;
    let mut state = state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match state.last_error.take() {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// Wait for queued Tonepoet host-clipboard publications to finish. Structured
/// metadata copy uses this as an ordering barrier before returning control to
/// the terminal. The logical clipboard was retained synchronously before the
/// publication was queued; OSC 52 completion cannot imply terminal acceptance.
pub(crate) fn wait_for_host_clipboard_publication() -> Result<(), String> {
    wait_for_host_clipboard_write_state(write_state(), CLIPBOARD_WRITE_DRAIN_TIMEOUT)
}

fn wait_for_prior_host_clipboard_writes() -> Result<(), String> {
    wait_for_host_clipboard_write_state(write_state(), CLIPBOARD_WRITE_DRAIN_TIMEOUT)
}

/// Launch a bounded host clipboard read. The generation and semantic target
/// prevent a late completion from mutating a different editor.
pub(crate) fn request_host_clipboard_paste(
    tx: mpsc::Sender<AppMessage>,
    generation: u64,
    target: HostClipboardPasteTarget,
) {
    request_host_clipboard_paste_inner(tx, generation, target);
}

fn request_host_clipboard_paste_inner(
    tx: mpsc::Sender<AppMessage>,
    generation: u64,
    target: HostClipboardPasteTarget,
) {
    // Clipboard acquisition is asynchronous. Capture the logical generation
    // before spawning so a newer Tonepoet copy or terminal paste can supersede
    // this request even when the editor/focus identity itself has not changed.
    let logical_generation_at_request = tui_file_picker::logical_clipboard_snapshot()
        .map(|snapshot| snapshot.generation);
    let fallback_tx = tx.clone();
    let fallback_target = target.clone();
    let spawn_result = std::thread::Builder::new()
        .name("tonepoet-host-clipboard-read".to_string())
        .spawn(move || {
            let result = wait_for_prior_host_clipboard_writes().and_then(|()| {
                let env = ClipboardEnvironment::detect();
                let backend = RealClipboardBackend;
                let retained = tui_file_picker::logical_clipboard_snapshot();
                resolve_command_clipboard_read(
                    &backend,
                    &env,
                    retained.as_ref().map(|snapshot| snapshot.text.as_str()),
                    ClipboardOperation::Read,
                )
            });
            let logical_generation_now = tui_file_picker::logical_clipboard_snapshot()
                .map(|snapshot| snapshot.generation);
            if logical_generation_now != logical_generation_at_request {
                record_attempt(
                    ClipboardOperation::Read,
                    "stale-logical-generation",
                    Ok("discarded because a newer logical clipboard value superseded the request"
                        .to_string()),
                );
                return;
            }
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
        "Environment\n  WAYLAND_DISPLAY={}\n  DISPLAY={}\n  XDG_RUNTIME_DIR={}\n  XAUTHORITY={}\n  remote-session={}\n  TMUX={}\n  STY={}\n  TERM={}\n\nDetected transports\n  write: {}{}\n  read: {}\n\nLive self-test\n  {}\n\nRecent attempts",
        ClipboardEnvironment::display_value_with_source(
            &env.wayland_display,
            &env.wayland_display_source,
        ),
        ClipboardEnvironment::display_value_with_source(&env.display, &env.display_source),
        ClipboardEnvironment::display_value(&env.xdg_runtime_dir),
        ClipboardEnvironment::display_value(&env.xauthority),
        if env.remote_session { "yes" } else { "no" },
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
            match backend.write_command(candidate.program, &candidate.args, text.as_bytes(), env) {
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


fn resolve_command_clipboard_read(
    backend: &impl ClipboardBackend,
    env: &ClipboardEnvironment,
    retained: Option<&str>,
    operation: ClipboardOperation,
) -> Result<String, String> {
    if native_read_candidates(backend, env).is_empty() {
        if let Some(text) = retained {
            record_attempt(
                operation,
                "retained-logical",
                Ok(format!("{} bytes; no readable host transport", text.len())),
            );
            return Ok(text.to_string());
        }
    }

    // If a readable native backend exists, it is authoritative. An unexpected
    // failure must surface rather than silently substituting retained data.
    read_host_clipboard_with(backend, env, operation)
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
        match backend.read_command(candidate.program, &candidate.args, env) {
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
    #[cfg(target_os = "macos")]
    if backend.command_exists("pbcopy", env) {
        candidates.push(ClipboardCommand {
            program: "pbcopy",
            args: Vec::new(),
        });
    }
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
    #[cfg(target_os = "macos")]
    if backend.command_exists("pbpaste", env) {
        candidates.push(ClipboardCommand {
            program: "pbpaste",
            args: Vec::new(),
        });
    }
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
    let mut reason = if cfg!(target_os = "macos") {
        "no usable pbcopy transport".to_string()
    } else if env.wayland_display.is_none() && env.display.is_none() {
        "no readable Wayland/X11 display could be resolved from the process or same-UID session ancestry; native clipboard helpers were not eligible".to_string()
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
    let mut reason = if cfg!(target_os = "macos") {
        "host clipboard read requires a usable pbpaste transport".to_string()
    } else if env.wayland_display.is_none() && env.display.is_none() {
        if env.remote_session {
            "host clipboard read could not resolve a readable Wayland/X11 display from this remote session lineage; write-only OSC 52 cannot service Ctrl+V/Ctrl+P"
                .to_string()
        } else {
            "host clipboard read could not resolve a readable Wayland/X11 display from the process or same-UID session ancestry"
                .to_string()
        }
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

fn run_clipboard_write(
    program: &str,
    args: &[&str],
    payload: &[u8],
    env: &ClipboardEnvironment,
) -> Result<(), String> {
    // Clipboard writers such as xclip may fork a long-lived selection owner.
    // Piping stderr and waiting for EOF would then wait on every descendant
    // that inherited the descriptor, defeating the command timeout and
    // stranding the coalescing worker. Native write failures remain actionable
    // through the helper name and exit status; foreground reads still retain
    // bounded stdout/stderr capture below.
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    apply_clipboard_command_environment(&mut command, env);
    let mut child = command
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

fn run_clipboard_read(
    program: &str,
    args: &[&str],
    env: &ClipboardEnvironment,
) -> Result<String, String> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    apply_clipboard_command_environment(&mut command, env);
    let mut child = command
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

fn apply_clipboard_command_environment(command: &mut Command, env: &ClipboardEnvironment) {
    if let Some(value) = env.wayland_display.as_ref() {
        command.env("WAYLAND_DISPLAY", value);
    }
    if let Some(value) = env.display.as_ref() {
        command.env("DISPLAY", value);
    }
    if let Some(value) = env.xdg_runtime_dir.as_ref() {
        command.env("XDG_RUNTIME_DIR", value);
    }
    if let Some(value) = env.xauthority.as_ref() {
        command.env("XAUTHORITY", value);
    }
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
            _env: &ClipboardEnvironment,
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

        fn read_command(
            &self,
            program: &str,
            _args: &[&str],
            _env: &ClipboardEnvironment,
        ) -> Result<String, String> {
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
            display: Some(OsString::from(":0")),
            term: Some(OsString::from("xterm-256color")),
            display_source: Some("test fixture".to_string()),
            ..ClipboardEnvironment::default()
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_ancestor_environment_parser_extracts_clipboard_context() {
        let parsed = parse_linux_ancestor_environment(
            b"PATH=/bin\0WAYLAND_DISPLAY=wayland-7\0XDG_RUNTIME_DIR=/run/user/1000\0DISPLAY=:3\0XAUTHORITY=/run/user/1000/xauth\0SSH_CONNECTION=client server\0",
        );
        assert_eq!(
            parsed.wayland_display.as_deref(),
            Some(std::ffi::OsStr::new("wayland-7"))
        );
        assert_eq!(
            parsed.xdg_runtime_dir.as_deref(),
            Some(std::ffi::OsStr::new("/run/user/1000"))
        );
        assert_eq!(
            parsed.display.as_deref(),
            Some(std::ffi::OsStr::new(":3"))
        );
        assert_eq!(
            parsed.xauthority.as_deref(),
            Some(std::ffi::OsStr::new("/run/user/1000/xauth"))
        );
        assert!(parsed.remote_session);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn nearest_same_uid_ancestor_fills_missing_display_context_without_overriding_inherited_values() {
        let mut env = ClipboardEnvironment {
            display: Some(OsString::from(":42")),
            display_source: Some("process environment".to_string()),
            xdg_runtime_dir: Some(OsString::from("/run/user/1000/sandbox")),
            ..ClipboardEnvironment::default()
        };
        let nearest = LinuxAncestorEnvironment {
            wayland_display: Some(OsString::from("wayland-2")),
            display: Some(OsString::from(":7")),
            xdg_runtime_dir: Some(OsString::from("/run/user/1000")),
            xauthority: Some(OsString::from("/tmp/nearest-xauth")),
            remote_session: false,
        };
        merge_linux_ancestor_environment(&mut env, 4242, &nearest);

        assert_eq!(env.display.as_deref(), Some(std::ffi::OsStr::new(":42")));
        assert_eq!(
            env.display_source.as_deref(),
            Some("process environment")
        );
        assert_eq!(
            env.wayland_display.as_deref(),
            Some(std::ffi::OsStr::new("wayland-2"))
        );
        assert_eq!(
            env.wayland_display_source.as_deref(),
            Some("same-UID ancestor pid 4242")
        );
        assert_eq!(
            env.xdg_runtime_dir.as_deref(),
            Some(std::ffi::OsStr::new("/run/user/1000"))
        );
        assert!(env.xauthority.is_none());

        let farther = LinuxAncestorEnvironment {
            wayland_display: Some(OsString::from("wayland-9")),
            display: Some(OsString::from(":9")),
            xdg_runtime_dir: Some(OsString::from("/run/user/1000/farther")),
            xauthority: Some(OsString::from("/tmp/farther-xauth")),
            remote_session: false,
        };
        merge_linux_ancestor_environment(&mut env, 3131, &farther);
        assert_eq!(
            env.wayland_display.as_deref(),
            Some(std::ffi::OsStr::new("wayland-2"))
        );
        assert_eq!(env.display.as_deref(), Some(std::ffi::OsStr::new(":42")));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn recovered_x11_display_makes_native_clipboard_read_eligible() {
        let mut env = ClipboardEnvironment {
            xauthority: Some(OsString::from("/tmp/stale-xauth")),
            ..ClipboardEnvironment::default()
        };
        let ancestor = LinuxAncestorEnvironment {
            display: Some(OsString::from(":7")),
            xauthority: Some(OsString::from("/tmp/tonepoet-xauth")),
            ..LinuxAncestorEnvironment::default()
        };
        merge_linux_ancestor_environment(&mut env, 4242, &ancestor);

        let mut backend = FakeBackend::default();
        backend.programs.insert("xclip".to_string());
        backend
            .read_results
            .insert("xclip".to_string(), Ok("Duke".to_string()));
        let value = read_host_clipboard_with(
            &backend,
            &env,
            ClipboardOperation::Diagnostic,
        )
        .expect("ancestor-recovered DISPLAY should admit xclip");
        assert_eq!(value, "Duke");
        assert_eq!(
            env.xauthority.as_deref(),
            Some(std::ffi::OsStr::new("/tmp/tonepoet-xauth"))
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn unique_wayland_runtime_socket_is_inferred_only_for_local_sessions() {
        let temp = tempfile::tempdir().expect("tempdir");
        let socket_path = temp.path().join("wayland-5");
        let _listener = std::os::unix::net::UnixListener::bind(&socket_path)
            .expect("wayland fixture socket");
        std::fs::write(temp.path().join("wayland-5.lock"), b"fixture")
            .expect("lock fixture");

        let mut local = ClipboardEnvironment {
            xdg_runtime_dir: Some(temp.path().as_os_str().to_os_string()),
            ..ClipboardEnvironment::default()
        };
        infer_wayland_display_from_runtime(&mut local);
        assert_eq!(
            local.wayland_display.as_deref(),
            Some(std::ffi::OsStr::new("wayland-5"))
        );

        let mut remote = ClipboardEnvironment {
            xdg_runtime_dir: Some(temp.path().as_os_str().to_os_string()),
            remote_session: true,
            ..ClipboardEnvironment::default()
        };
        infer_wayland_display_from_runtime(&mut remote);
        assert!(remote.wayland_display.is_none());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn multiple_wayland_runtime_sockets_are_not_guessed() {
        let temp = tempfile::tempdir().expect("tempdir");
        let _first = std::os::unix::net::UnixListener::bind(temp.path().join("wayland-0"))
            .expect("first wayland fixture socket");
        let _second = std::os::unix::net::UnixListener::bind(temp.path().join("wayland-1"))
            .expect("second wayland fixture socket");

        let mut env = ClipboardEnvironment {
            xdg_runtime_dir: Some(temp.path().as_os_str().to_os_string()),
            ..ClipboardEnvironment::default()
        };
        infer_wayland_display_from_runtime(&mut env);
        assert!(env.wayland_display.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn native_clipboard_helper_receives_resolved_display_environment() {
        let env = ClipboardEnvironment {
            wayland_display: Some(OsString::from("wayland-4")),
            display: Some(OsString::from(":8")),
            xdg_runtime_dir: Some(OsString::from("/run/user/1000")),
            xauthority: Some(OsString::from("/tmp/tonepoet-xauth")),
            ..ClipboardEnvironment::default()
        };
        let value = run_clipboard_read(
            "sh",
            &[
                "-c",
                "printf '%s|%s|%s|%s' \"$WAYLAND_DISPLAY\" \"$DISPLAY\" \"$XDG_RUNTIME_DIR\" \"$XAUTHORITY\"",
            ],
            &env,
        )
        .expect("shell fixture should inherit resolved display context");
        assert_eq!(
            value,
            "wayland-4|:8|/run/user/1000|/tmp/tonepoet-xauth"
        );
    }

    #[cfg(target_os = "macos")]
    fn macos_env() -> ClipboardEnvironment {
        ClipboardEnvironment {
            term: Some(OsString::from("xterm-256color")),
            ..ClipboardEnvironment::default()
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
            env: &ClipboardEnvironment,
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
            run_clipboard_write("sh", &args, payload, env)
        }

        fn read_command(
            &self,
            _program: &str,
            _args: &[&str],
            _env: &ClipboardEnvironment,
        ) -> Result<String, String> {
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
        run_clipboard_write(
            "sh",
            &["-c", "cat >/dev/null; sleep 5 &"],
            b"Duke",
            &x11_env(),
        )
        .expect("immediate clipboard owner parent exits successfully");
        assert!(
            started.elapsed() < CLIPBOARD_COMMAND_TIMEOUT,
            "native write waited for a background descendant instead of the immediate child: {:?}",
            started.elapsed(),
        );
    }

    #[test]
    fn metadata_copy_publication_barrier_waits_for_queued_write_before_returning() {
        let state = std::sync::Arc::new(Mutex::new(HostClipboardWriteState {
            pending: Some("structured metadata envelope".to_string()),
            worker_running: true,
            last_error: None,
        }));
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let worker_state = std::sync::Arc::clone(&state);
        let worker = std::thread::spawn(move || {
            ready_tx.send(()).expect("signal waiter");
            release_rx.recv().expect("release publication");
            let mut state = worker_state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.pending = None;
            state.worker_running = false;
        });
        ready_rx.recv().expect("publication worker ready");
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(40));
            release_tx.send(()).expect("release publication worker");
        });

        let started = Instant::now();
        wait_for_host_clipboard_write_completion(state.as_ref(), Duration::from_secs(1))
            .expect("publication barrier drains");
        assert!(
            started.elapsed() >= Duration::from_millis(30),
            "metadata copy returned before its queued host publication drained",
        );
        releaser.join().expect("publication releaser");
        worker.join().expect("publication worker");
    }

    #[test]
    fn write_completion_diagnostic_surfaces_and_consumes_write_failure() {
        let state = Mutex::new(HostClipboardWriteState {
            pending: None,
            worker_running: false,
            last_error: Some("xclip exited with status 1".to_string()),
        });

        let error = wait_for_host_clipboard_write_completion(&state, Duration::from_millis(1))
            .expect_err("failed publication must not be reported as a successful copy");
        assert_eq!(error, "xclip exited with status 1");
        wait_for_host_clipboard_write_completion(&state, Duration::from_millis(1))
            .expect("the reported failure must not poison unrelated later clipboard reads");
    }

    #[test]
    fn logical_generation_change_marks_an_in_flight_read_stale() {
        tui_file_picker::with_scoped_shared_text_clipboard("A", || {
            let request_generation = tui_file_picker::logical_clipboard_snapshot()
                .map(|snapshot| snapshot.generation);
            let _ = tui_file_picker::retain_logical_clipboard_text("B");
            let current_generation = tui_file_picker::logical_clipboard_snapshot()
                .map(|snapshot| snapshot.generation);
            assert_ne!(current_generation, request_generation);
        });
    }

    #[test]
    fn command_paste_uses_retained_value_only_when_no_readable_transport_exists() {
        let backend = FakeBackend::default();
        let env = ClipboardEnvironment::default();
        let resolved = resolve_command_clipboard_read(
            &backend,
            &env,
            Some("retained Tonepoet value"),
            ClipboardOperation::Diagnostic,
        )
        .expect("retained fallback should satisfy a headless paste");
        assert_eq!(resolved, "retained Tonepoet value");
    }

    #[test]
    fn command_paste_without_transport_or_retained_value_is_unavailable() {
        let backend = FakeBackend::default();
        let env = ClipboardEnvironment::default();
        let error = resolve_command_clipboard_read(
            &backend,
            &env,
            None,
            ClipboardOperation::Diagnostic,
        )
        .expect_err("no readable backend and no retained value must fail");
        assert!(error.contains("host clipboard read"));
    }

    #[test]
    fn command_paste_does_not_hide_failure_of_an_existing_authoritative_backend() {
        let mut backend = FakeBackend::default();
        backend.programs.insert("xclip".to_string());
        backend
            .read_results
            .insert("xclip".to_string(), Err("backend failed".to_string()));
        let error = resolve_command_clipboard_read(
            &backend,
            &x11_env(),
            Some("stale retained value"),
            ClipboardOperation::Diagnostic,
        )
        .expect_err("a present but failing host backend must remain authoritative");
        assert!(error.contains("backend failed"));
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
            last_error: None,
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

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_without_display_variables_admits_pbcopy_and_pbpaste() {
        let mut backend = FakeBackend::default();
        backend.programs.insert("pbcopy".to_string());
        backend.programs.insert("pbpaste".to_string());
        let env = macos_env();

        let writes = native_write_candidates(&backend, &env);
        let reads = native_read_candidates(&backend, &env);
        assert_eq!(
            writes
                .iter()
                .map(|candidate| candidate.program)
                .collect::<Vec<_>>(),
            vec!["pbcopy"],
        );
        assert_eq!(
            reads
                .iter()
                .map(|candidate| candidate.program)
                .collect::<Vec<_>>(),
            vec!["pbpaste"],
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_pbpaste_success_is_returned() {
        let mut backend = FakeBackend::default();
        backend.programs.insert("pbpaste".to_string());
        backend
            .read_results
            .insert("pbpaste".to_string(), Ok("Duke".to_string()));

        let value = read_host_clipboard_with(
            &backend,
            &macos_env(),
            ClipboardOperation::Diagnostic,
        )
        .expect("pbpaste read");
        assert_eq!(value, "Duke");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_pbcopy_failure_falls_back_to_osc52() {
        let mut backend = FakeBackend::default();
        backend.programs.insert("pbcopy".to_string());
        backend
            .write_results
            .insert("pbcopy".to_string(), Err("pasteboard unavailable".to_string()));

        let outcome = write_host_clipboard_with(
            &backend,
            &macos_env(),
            "Duke",
            ClipboardOperation::Diagnostic,
        )
        .expect("OSC52 fallback after pbcopy failure");
        assert_eq!(outcome.transport, "OSC52");
        assert_eq!(
            backend
                .writes
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .as_slice(),
            &["pbcopy".to_string()]
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_write_failure_diagnostic_names_pbcopy_not_linux_display_servers() {
        let mut backend = FakeBackend::default();
        backend.programs.insert("pbcopy".to_string());
        backend
            .write_results
            .insert("pbcopy".to_string(), Err("pasteboard unavailable".to_string()));
        backend.osc_result = Err("no tty".to_string());

        let error = write_host_clipboard_with(
            &backend,
            &macos_env(),
            "Duke",
            ClipboardOperation::Diagnostic,
        )
        .expect_err("pbcopy and OSC52 failure must be actionable");
        assert!(error.contains("pbcopy"));
        assert!(!error.contains("DISPLAY"));
        assert!(!error.contains("WAYLAND_DISPLAY"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_read_failure_diagnostic_names_pbpaste_not_linux_display_servers() {
        let mut backend = FakeBackend::default();
        backend.programs.insert("pbpaste".to_string());
        backend
            .read_results
            .insert("pbpaste".to_string(), Err("pasteboard unavailable".to_string()));

        let error = read_host_clipboard_with(
            &backend,
            &macos_env(),
            ClipboardOperation::Diagnostic,
        )
        .expect_err("pbpaste failure must be actionable");
        assert!(error.contains("pbpaste"));
        assert!(!error.contains("DISPLAY"));
        assert!(!error.contains("WAYLAND_DISPLAY"));
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
