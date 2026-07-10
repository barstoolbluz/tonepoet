//! Launch an external text editor, suspending the TUI while it runs.

use std::path::Path;
use std::process::Command;

/// Open a file in the user's preferred editor.
///
/// Suspends the terminal (raw mode off, cursor visible), runs the editor,
/// then restores the terminal for the TUI. Returns `Ok(true)` if the
/// editor exited successfully, `Ok(false)` if it exited with an error
/// (e.g., user killed it), or `Err` if no editor could be found.
pub fn open_in_editor(path: &Path) -> Result<bool, String> {
    let editor_str = detect_editor()?;
    let (program, args) = split_command(&editor_str);

    // Suspend TUI: restore normal terminal mode so the editor can run.
    let _ = crossterm::terminal::disable_raw_mode();
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::event::DisableMouseCapture,
        crossterm::event::DisableBracketedPaste,
        crossterm::cursor::Show,
        crossterm::terminal::LeaveAlternateScreen,
    );

    // Run the editor, blocking until it exits. DELIBERATE stdin inheritance:
    // the user's $EDITOR needs the terminal. This is the documented exemption
    // from the subprocess stdin-nulling convention (see the sentinel test
    // tests/subprocess_stdin_convention.rs).
    let status = Command::new(program)
        .args(&args)
        .arg(path)
        .status()
        .map_err(|e| format!("failed to run {}: {}", editor_str, e))?;

    // Restore TUI: re-enter raw mode and alternate screen.
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::terminal::EnterAlternateScreen,
        crossterm::event::EnableMouseCapture,
        crossterm::event::EnableBracketedPaste,
        crossterm::cursor::Hide,
    );
    let _ = crossterm::terminal::enable_raw_mode();

    // Force a full redraw on the next frame by clearing the terminal.
    // Without this, ratatui's diff-based rendering may leave artifacts
    // from the editor's output.
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::terminal::Clear(crossterm::terminal::ClearType::All),
    );

    Ok(status.success())
}

/// Open a file in read-only view mode.
///
/// Same TUI suspend/restore as `open_in_editor`, but passes a
/// read-only flag to the editor (vim: `-R`, nano: `-v`). Falls back
/// to `less` if no editor is found.
pub fn open_in_viewer(path: &Path) -> Result<bool, String> {
    let editor_str = detect_editor_for_view()?;
    let (program, args) = split_command(&editor_str);

    // Determine read-only flag based on the program basename.
    let editor_base = std::path::Path::new(program)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(program)
        .to_lowercase();
    let readonly_flag: Option<&str> = match editor_base.as_str() {
        "vim" | "vi" | "nvim" => Some("-R"),
        "nano" => Some("-v"),
        "less" | "more" | "bat" | "cat" => None, // inherently read-only
        _ => Some("-R"),                         // best guess (vim-compatible)
    };

    // Suspend TUI.
    let _ = crossterm::terminal::disable_raw_mode();
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::event::DisableMouseCapture,
        crossterm::event::DisableBracketedPaste,
        crossterm::cursor::Show,
        crossterm::terminal::LeaveAlternateScreen,
    );

    // Build and run command.
    let mut cmd = Command::new(program);
    cmd.args(&args);
    if let Some(flag) = readonly_flag {
        cmd.arg(flag);
    }
    cmd.arg(path);
    let status = cmd
        .status()
        .map_err(|e| format!("failed to run {}: {}", editor_str, e))?;

    // Restore TUI.
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::terminal::EnterAlternateScreen,
        crossterm::event::EnableMouseCapture,
        crossterm::event::EnableBracketedPaste,
        crossterm::cursor::Hide,
    );
    let _ = crossterm::terminal::enable_raw_mode();
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::terminal::Clear(crossterm::terminal::ClearType::All),
    );

    Ok(status.success())
}

/// Split a command string into program and arguments.
///
/// Handles `EDITOR="vim -u NONE"`, `EDITOR="code --wait"`, etc.
/// The first whitespace-separated token is the program; the rest are args.
fn split_command(cmd: &str) -> (&str, Vec<&str>) {
    let mut parts = cmd.split_whitespace();
    let program = parts.next().unwrap_or(cmd);
    let args: Vec<&str> = parts.collect();
    (program, args)
}

/// Detect editor for viewing. Prefers $EDITOR/$VISUAL, falls back to
/// `less` (better for read-only viewing than nano/vim).
fn detect_editor_for_view() -> Result<String, String> {
    // Check environment variables first.
    for var in &["EDITOR", "VISUAL"] {
        if let Ok(editor) = std::env::var(var) {
            if !editor.is_empty() {
                return Ok(editor);
            }
        }
    }

    // For viewing, prefer `less` over editors.
    for candidate in &["less", "nano", "vim", "vi", "more"] {
        if which(candidate) {
            return Ok(candidate.to_string());
        }
    }

    Err("no viewer found — set $EDITOR".to_string())
}

/// Detect the user's preferred editor from environment variables,
/// falling back to common editors on the PATH.
fn detect_editor() -> Result<String, String> {
    // Check environment variables.
    for var in &["EDITOR", "VISUAL"] {
        if let Ok(editor) = std::env::var(var) {
            if !editor.is_empty() {
                return Ok(editor);
            }
        }
    }

    // Probe for common editors.
    for candidate in &["nano", "vim", "vi"] {
        if which(candidate) {
            return Ok(candidate.to_string());
        }
    }

    Err("no editor found — set $EDITOR".to_string())
}

/// Check if a command exists on the PATH.
fn which(cmd: &str) -> bool {
    Command::new("which")
        .arg(cmd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}
