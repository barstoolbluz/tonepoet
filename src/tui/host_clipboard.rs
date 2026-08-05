//! Best-effort host clipboard integration.
//!
//! Internal Tonepoet clipboards remain authoritative. Host writes are
//! coalesced onto a background worker so a missing display server, a wedged
//! clipboard helper, or terminal passthrough never stalls the event/render
//! path. Reads are requested explicitly with Ctrl+Shift+V and return through
//! the ordinary TUI message channel.

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;

use super::message::{AppMessage, HostClipboardPasteTarget};

const NATIVE_CLIPBOARD_MAX_BYTES: usize = 1024 * 1024;
const OSC52_TEXT_CLIPBOARD_MAX_BYTES: usize = 64 * 1024;
const CLIPBOARD_COMMAND_TIMEOUT: Duration = Duration::from_secs(2);
const CLIPBOARD_POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Default)]
struct HostClipboardWriteState {
    pending: Option<String>,
    worker_running: bool,
}

static HOST_CLIPBOARD_WRITE_STATE: OnceLock<Mutex<HostClipboardWriteState>> = OnceLock::new();

fn write_state() -> &'static Mutex<HostClipboardWriteState> {
    HOST_CLIPBOARD_WRITE_STATE.get_or_init(|| Mutex::new(HostClipboardWriteState::default()))
}

/// Publication hook installed into `tui-file-picker`.
///
/// The in-process clipboard has already been updated before this function is
/// called. We therefore coalesce rapid writes and return immediately; host
/// integration can fail without changing copy/cut semantics.
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
            log::debug!("host clipboard worker could not start: {error}");
        }
    }
}

fn host_clipboard_write_worker() {
    loop {
        let next = {
            let mut state = write_state()
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

        if let Err(error) = write_host_clipboard_best_effort(&next) {
            log::debug!("host clipboard write unavailable: {error}");
        }
    }
}

/// Launch a non-blocking host clipboard read. The caller supplies a monotonic
/// generation and a semantic target; the reducer validates both before
/// mutating any live editor.
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
            let result = read_host_clipboard();
            let _ = tx.blocking_send(AppMessage::HostClipboardReadComplete {
                generation,
                target,
                result,
            });
        });

    if let Err(error) = spawn_result {
        let _ = fallback_tx.try_send(AppMessage::HostClipboardReadComplete {
            generation,
            target: fallback_target,
            result: Err(format!("could not start host clipboard reader: {error}")),
        });
    }
}

fn write_host_clipboard_best_effort(text: &str) -> Result<(), String> {
    if text.len() <= NATIVE_CLIPBOARD_MAX_BYTES {
        for candidate in native_write_candidates() {
            if run_clipboard_write(candidate.program, &candidate.args, text.as_bytes()).is_ok() {
                return Ok(());
            }
        }
    }

    write_osc52_clipboard_to_tty(text)
}

fn read_host_clipboard() -> Result<String, String> {
    let candidates = native_read_candidates();
    if candidates.is_empty() {
        return Err(
            "host clipboard read is unavailable (install wl-clipboard, xclip, or xsel)"
                .to_string(),
        );
    }

    let mut errors = Vec::new();
    for candidate in candidates {
        match run_clipboard_read(candidate.program, &candidate.args) {
            Ok(text) => return Ok(text),
            Err(error) => errors.push(format!("{}: {error}", candidate.program)),
        }
    }

    Err(format!(
        "host clipboard read failed ({})",
        errors.join("; ")
    ))
}

#[derive(Debug, Clone)]
struct ClipboardCommand {
    program: &'static str,
    args: Vec<&'static str>,
}

fn native_write_candidates() -> Vec<ClipboardCommand> {
    let mut candidates = Vec::new();
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        candidates.push(ClipboardCommand {
            program: "wl-copy",
            args: vec!["--type", "text/plain;charset=utf-8"],
        });
    }
    if std::env::var_os("DISPLAY").is_some() {
        candidates.push(ClipboardCommand {
            program: "xclip",
            args: vec!["-selection", "clipboard", "-in"],
        });
        candidates.push(ClipboardCommand {
            program: "xsel",
            args: vec!["--clipboard", "--input"],
        });
    }
    candidates
}

fn native_read_candidates() -> Vec<ClipboardCommand> {
    let mut candidates = Vec::new();
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        candidates.push(ClipboardCommand {
            program: "wl-paste",
            args: vec!["--no-newline", "--type", "text"],
        });
    }
    if std::env::var_os("DISPLAY").is_some() {
        candidates.push(ClipboardCommand {
            program: "xclip",
            args: vec!["-selection", "clipboard", "-out"],
        });
        candidates.push(ClipboardCommand {
            program: "xsel",
            args: vec!["--clipboard", "--output"],
        });
    }
    candidates
}

fn run_clipboard_write(program: &str, args: &[&str], payload: &[u8]) -> Result<(), String> {
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
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| error.to_string())?;

    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| format!("{program} did not provide stdout"))?;
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout
            .take((NATIVE_CLIPBOARD_MAX_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
    });
    let status = wait_for_child_with_timeout(&mut child, program);
    let bytes = reader
        .join()
        .map_err(|_| format!("{program} clipboard reader panicked"))?
        .map_err(|error| error.to_string())?;
    let status = status?;

    if !status.success() {
        return Err(format!("{program} exited with {status}"));
    }
    if bytes.len() > NATIVE_CLIPBOARD_MAX_BYTES {
        return Err(format!(
            "clipboard payload exceeds {} bytes",
            NATIVE_CLIPBOARD_MAX_BYTES
        ));
    }
    String::from_utf8(bytes).map_err(|_| "clipboard text is not valid UTF-8".to_string())
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

fn write_osc52_clipboard_to_tty(text: &str) -> Result<(), String> {
    let mut tty = OpenOptions::new()
        .write(true)
        .open("/dev/tty")
        .map_err(|error| error.to_string())?;
    write_osc52_clipboard_to_with_multiplexer(
        &mut tty,
        text,
        std::env::var_os("TMUX").is_some(),
        std::env::var_os("STY").is_some(),
    )
    .map_err(|error| error.to_string())?
    .then_some(())
    .ok_or_else(|| {
        format!(
            "clipboard payload exceeds the OSC 52 limit of {} bytes",
            OSC52_TEXT_CLIPBOARD_MAX_BYTES
        )
    })
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

    #[test]
    fn osc52_encoding_is_exact_for_plain_tmux_and_screen_paths() {
        let mut plain = Vec::new();
        assert!(write_osc52_clipboard_to_with_multiplexer(
            &mut plain,
            "Duke",
            false,
            false,
        )
        .expect("plain OSC 52"));
        assert_eq!(plain, b"\x1b]52;c;RHVrZQ==\x07");

        let mut tmux = Vec::new();
        assert!(write_osc52_clipboard_to_with_multiplexer(
            &mut tmux,
            "Duke",
            true,
            false,
        )
        .expect("tmux OSC 52"));
        assert_eq!(tmux, b"\x1bPtmux;\x1b\x1b]52;c;RHVrZQ==\x07\x1b\\");

        let mut screen = Vec::new();
        assert!(write_osc52_clipboard_to_with_multiplexer(
            &mut screen,
            "Duke",
            false,
            true,
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
