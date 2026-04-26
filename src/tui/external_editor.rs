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
    let editor = detect_editor()?;

    // Suspend TUI: restore normal terminal mode so the editor can run.
    let _ = crossterm::terminal::disable_raw_mode();
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::cursor::Show,
        crossterm::terminal::LeaveAlternateScreen,
    );

    // Run the editor, blocking until it exits.
    let status = Command::new(&editor)
        .arg(path)
        .status()
        .map_err(|e| format!("failed to run {}: {}", editor, e))?;

    // Restore TUI: re-enter raw mode and alternate screen.
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::terminal::EnterAlternateScreen,
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
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}
