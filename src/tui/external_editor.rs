//! Launch an external text editor, suspending the TUI while it runs.

use std::path::Path;
use std::process::Command;

trait TuiTerminalBackend {
    fn suspend(&self);
    fn restore(&self);
}

#[derive(Clone, Copy)]
struct CrosstermTerminalBackend;

impl TuiTerminalBackend for CrosstermTerminalBackend {
    fn suspend(&self) {
        // Suspend TUI: restore normal terminal mode so the editor can run.
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::event::DisableMouseCapture,
            crossterm::event::DisableBracketedPaste,
            crossterm::cursor::Show,
            crossterm::terminal::LeaveAlternateScreen,
        );
    }

    fn restore(&self) {
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
    }
}

struct TuiRestoreGuard<B: TuiTerminalBackend = CrosstermTerminalBackend> {
    backend: B,
    restore: bool,
}

impl TuiRestoreGuard<CrosstermTerminalBackend> {
    fn suspend() -> Self {
        Self::suspend_with(CrosstermTerminalBackend)
    }
}

impl<B: TuiTerminalBackend> TuiRestoreGuard<B> {
    fn suspend_with(backend: B) -> Self {
        backend.suspend();
        Self {
            backend,
            restore: true,
        }
    }

    fn restore_now(&mut self) {
        if !self.restore {
            return;
        }
        self.restore = false;
        self.backend.restore();
    }
}

impl<B: TuiTerminalBackend> Drop for TuiRestoreGuard<B> {
    fn drop(&mut self) {
        self.restore_now();
    }
}

/// Remove stale per-process embedded-CUESHEET edit buffers left by crashed
/// instances. Live and malformed process directories are retained: deletion is
/// best-effort and only occurs after a numeric PID is proven dead.
pub fn scavenge_stale_embedded_cuesheet_edit_dirs() {
    let root = std::env::temp_dir().join("tonepoet-embedded-cuesheet-edits");
    let Ok(entries) = std::fs::read_dir(&root) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(pid_text) = name.strip_prefix("process-") else {
            continue;
        };
        let Ok(pid) = pid_text.parse::<u32>() else {
            continue;
        };
        if crate::convert::queue_expansion::process_id_is_live(pid) {
            continue;
        }
        if let Err(err) = std::fs::remove_dir_all(entry.path()) {
            log::debug!(
                "failed to remove stale embedded-CUESHEET edit directory {}: {}",
                entry.path().display(),
                err
            );
        }
    }
    let _ = std::fs::remove_dir(&root);
}

/// Open a file in the user's preferred editor.
///
/// Suspends the terminal (raw mode off, cursor visible), runs the editor,
/// then restores the terminal for the TUI. Returns `Ok(true)` if the
/// editor exited successfully, `Ok(false)` if it exited with an error
/// (e.g., user killed it), or `Err` if no editor could be found.
pub fn open_in_editor(path: &Path) -> Result<bool, String> {
    let editor_str = detect_editor()?;
    open_in_editor_with_terminal(path, &editor_str, CrosstermTerminalBackend)
}

fn open_in_editor_with_terminal<B: TuiTerminalBackend>(
    path: &Path,
    editor_str: &str,
    terminal: B,
) -> Result<bool, String> {
    let (program, args) = split_command(editor_str);

    // Admission happens before terminal suspension so a busy file never tears
    // down the TUI. Keep the parent-side guard alive for the complete editor
    // lifetime; unlike the old supervised launch, no lease descriptor is ever
    // duplicated into or exported to the child process.
    let claim = crate::concurrency::PathClaim::resolve(
        path,
        crate::concurrency::ClaimMode::Write,
        crate::concurrency::ClaimScope::Exact,
    )?;
    let mutation_claim = crate::concurrency::MutationClaimGuard::acquire_ephemeral(vec![claim])?;
    let admitted_path = mutation_claim
        .claims()
        .first()
        .map(|claim| claim.identity.resolved_io_path.clone())
        .ok_or_else(|| "editor mutation admission produced no path claim".to_string())?;

    // Validate and exact-open the executable before disturbing the terminal.
    // The CLOEXEC handle stays parent-owned through spawn/status, just like the
    // mutation claim, and is never exported to the editor.
    let pinned = pin_editor_executable(program)
        .map_err(|e| format!("failed to run {}: {}", editor_str, e))?;

    let mut terminal_restore = TuiRestoreGuard::suspend_with(terminal);

    // An interactive editor is intentionally a normal foreground child. It
    // inherits tonepoet's process group and terminal stdio, exactly like the
    // proven read-only viewer path. Process-tree detachment is actively wrong
    // here: a background process group cannot interact with the controlling
    // terminal without SIGTTIN/SIGTTOU job-control stops.
    //
    // `_mutation_claim` is parent-owned RAII state and remains live across the
    // blocking `status()` call. This preserves cross-session WRITE exclusion
    // without exposing a lifetime descriptor to the editor.
    let _mutation_claim = mutation_claim;
    let status = run_foreground_interactive_editor(&pinned, &args, &admitted_path);

    // Restore before interpreting the child's result, so even a spawn failure
    // returns to a live TUI while the parent still owns the WRITE claim. A
    // panic anywhere above is still covered by `TuiRestoreGuard::drop`.
    terminal_restore.restore_now();
    let status = status.map_err(|e| format!("failed to run {}: {}", editor_str, e))?;
    Ok(status.success())
}

struct PinnedEditorExecutable {
    path: std::path::PathBuf,
    #[cfg(unix)]
    _file: std::fs::File,
}

fn pin_editor_executable(program: &str) -> Result<PinnedEditorExecutable, String> {
    let path = resolve_editor_program(program)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(&path)
            .map_err(|error| format!("open exact editor executable {}: {error}", path.display()))?;
        if !file
            .metadata()
            .map_err(|error| format!("stat editor executable {}: {error}", path.display()))?
            .is_file()
        {
            return Err(format!(
                "editor executable is not a regular file: {}",
                path.display()
            ));
        }
        return Ok(PinnedEditorExecutable { path, _file: file });
    }

    #[cfg(not(unix))]
    {
        if !std::fs::metadata(&path)
            .map_err(|error| format!("stat editor executable {}: {error}", path.display()))?
            .is_file()
        {
            return Err(format!(
                "editor executable is not a regular file: {}",
                path.display()
            ));
        }
        Ok(PinnedEditorExecutable { path })
    }
}

fn run_foreground_interactive_editor(
    pinned: &PinnedEditorExecutable,
    args: &[&str],
    path: &Path,
) -> Result<std::process::ExitStatus, String> {
    // Keep the exact-open CLOEXEC handle alive through spawn/status. The child
    // receives no claim or executable-pin descriptors; only inherited terminal
    // stdio plus the ordinary argv path. DELIBERATE stdin inheritance: the
    // foreground editor must own the interactive terminal.
    Command::new(&pinned.path)
        .args(args)
        .arg(path)
        .status()
        .map_err(|error| error.to_string())
}

fn resolve_editor_program(program: &str) -> Result<std::path::PathBuf, String> {
    let path = std::path::PathBuf::from(program);
    let candidate = if path.is_absolute() || path.components().count() > 1 {
        path
    } else {
        let search = std::env::var_os("PATH")
            .ok_or_else(|| format!("PATH is unset while resolving editor '{program}'"))?;
        std::env::split_paths(&search)
            .map(|directory| directory.join(&path))
            .find(|candidate| candidate.is_file())
            .ok_or_else(|| format!("editor executable not found on PATH: {program}"))?
    };
    std::fs::canonicalize(&candidate)
        .map_err(|error| format!("canonicalize editor executable {}: {error}", candidate.display()))
}

/// Open a file in read-only view mode.
///
/// Same TUI suspend/restore as `open_in_editor`, but passes a
/// read-only flag to the editor (vim: `-R`, nano: `-v`). Falls back
/// to `less` if no editor is found.
pub fn open_in_viewer(path: &Path) -> Result<bool, String> {
    let editor_str = detect_editor_for_view()?;
    run_read_only_command(path, &editor_str)
}

fn run_read_only_command(path: &Path, editor_str: &str) -> Result<bool, String> {
    let (program, args) = split_command(editor_str);

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

    let mut terminal_restore = TuiRestoreGuard::suspend();

    // Build and run command. DELIBERATE stdin inheritance, same exemption as
    // the read-write editor spawn above: the user's $EDITOR needs the terminal
    // (see tests/subprocess_stdin_convention.rs). The restore guard protects
    // the TUI on fallible spawn as well as normal editor exit.
    let mut cmd = Command::new(program);
    cmd.args(&args);
    if let Some(flag) = readonly_flag {
        cmd.arg(flag);
    }
    cmd.arg(path);
    let status = cmd
        .status()
        .map_err(|e| format!("failed to run {}: {}", editor_str, e))?;

    terminal_restore.restore_now();
    Ok(status.success())
}

fn preferred_log_viewer_command(preference: crate::config::LogViewer) -> Option<&'static str> {
    (preference == crate::config::LogViewer::Bat).then_some("bat")
}

fn open_log_viewer_with(
    path: &Path,
    preference: crate::config::LogViewer,
    mut launch_read_only: impl FnMut(&Path, &str) -> Result<bool, String>,
    mut fallback: impl FnMut(&Path) -> Result<bool, String>,
) -> Result<bool, String> {
    if let Some(command) = preferred_log_viewer_command(preference) {
        match launch_read_only(path, command) {
            Ok(status) => return Ok(status),
            Err(error) => {
                log::debug!("bat log viewer unavailable at launch; falling back: {error}");
            }
        }
    }
    fallback(path)
}

/// Open a `.log` file through the configured strictly read-only policy.
/// `bat` is preferred by default; if its direct launch fails, the pre-existing
/// read-only viewer path remains the fallback rather than turning file
/// activation into an error.
fn open_log_viewer(
    path: &Path,
    preference: crate::config::LogViewer,
) -> Result<bool, String> {
    open_log_viewer_with(path, preference, run_read_only_command, open_in_viewer)
}

/// Open any read-only text target, applying the `.log` policy without changing
/// the behavior of other viewable text formats.
pub fn open_viewer_for_path(
    path: &Path,
    log_viewer: crate::config::LogViewer,
) -> Result<bool, String> {
    let is_log = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("log"));
    if is_log {
        open_log_viewer(path, log_viewer)
    } else {
        open_in_viewer(path)
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[derive(Default)]
    struct FakeTerminalState {
        raw_mode: std::sync::atomic::AtomicBool,
        alternate_screen: std::sync::atomic::AtomicBool,
        suspend_count: std::sync::atomic::AtomicUsize,
        restore_count: std::sync::atomic::AtomicUsize,
    }

    #[cfg(unix)]
    #[derive(Clone)]
    struct FakeTerminalBackend(std::sync::Arc<FakeTerminalState>);

    #[cfg(unix)]
    impl TuiTerminalBackend for FakeTerminalBackend {
        fn suspend(&self) {
            use std::sync::atomic::Ordering;
            self.0.raw_mode.store(false, Ordering::SeqCst);
            self.0.alternate_screen.store(false, Ordering::SeqCst);
            self.0.suspend_count.fetch_add(1, Ordering::SeqCst);
        }

        fn restore(&self) {
            use std::sync::atomic::Ordering;
            self.0.raw_mode.store(true, Ordering::SeqCst);
            self.0.alternate_screen.store(true, Ordering::SeqCst);
            self.0.restore_count.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[cfg(unix)]
    fn executable_test_script(path: &Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, body).expect("write editor fixture");
        let mut permissions = std::fs::metadata(path)
            .expect("stat editor fixture")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).expect("chmod editor fixture");
    }

    #[cfg(unix)]
    fn wait_for_file(path: &Path) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !path.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for {}",
                path.display()
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[cfg(unix)]
    #[test]
    fn foreground_editor_holds_write_claim_and_restores_terminal_after_signal_exit() {
        use std::sync::atomic::Ordering;

        let _coordination = crate::concurrency::scoped_test_coordination_root();
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("notes.txt");
        std::fs::write(&target, b"notes\n").expect("write target");
        let editor = dir.path().join("editor.sh");
        executable_test_script(
            &editor,
            r#"#!/bin/sh
set -eu
folder=$(dirname "$1")
self_pgid=$(ps -o pgid= -p $$ | tr -d ' ')
parent_pgid=$(ps -o pgid= -p "$PPID" | tr -d ' ')
printf '%s %s\n' "$self_pgid" "$parent_pgid" > "$folder/pgids"
: > "$folder/started"
while [ ! -e "$folder/release" ]; do sleep 0.01; done
kill -TERM $$
"#,
        );

        let terminal_state = std::sync::Arc::new(FakeTerminalState::default());
        terminal_state.raw_mode.store(true, Ordering::SeqCst);
        terminal_state.alternate_screen.store(true, Ordering::SeqCst);
        let terminal = FakeTerminalBackend(terminal_state.clone());
        let thread_target = target.clone();
        let editor_command = editor.to_string_lossy().into_owned();
        let editor_thread = std::thread::spawn(move || {
            open_in_editor_with_terminal(&thread_target, &editor_command, terminal)
        });

        wait_for_file(&dir.path().join("started"));
        let pgids = std::fs::read_to_string(dir.path().join("pgids")).expect("read pgids");
        let pgids: Vec<&str> = pgids.split_whitespace().collect();
        assert_eq!(pgids.len(), 2);
        assert_eq!(
            pgids[0], pgids[1],
            "interactive editor must inherit tonepoet's foreground process group"
        );

        let competing_claim = crate::concurrency::PathClaim::resolve(
            &target,
            crate::concurrency::ClaimMode::Write,
            crate::concurrency::ClaimScope::Exact,
        )
        .expect("resolve competing claim");
        let busy = crate::concurrency::MutationClaimGuard::acquire_ephemeral(vec![competing_claim])
            .expect_err("editor must keep the WRITE claim live while it runs");
        assert!(busy.contains("live owner"), "unexpected busy error: {busy}");

        std::fs::write(dir.path().join("release"), b"go").expect("release editor");
        let result = editor_thread.join().expect("editor thread must not panic");
        assert_eq!(result, Ok(false), "signal-killed editor is a clean false result");
        assert!(terminal_state.raw_mode.load(Ordering::SeqCst));
        assert!(terminal_state.alternate_screen.load(Ordering::SeqCst));
        assert_eq!(terminal_state.suspend_count.load(Ordering::SeqCst), 1);
        assert_eq!(terminal_state.restore_count.load(Ordering::SeqCst), 1);

        let released_claim = crate::concurrency::PathClaim::resolve(
            &target,
            crate::concurrency::ClaimMode::Write,
            crate::concurrency::ClaimScope::Exact,
        )
        .expect("resolve released claim");
        crate::concurrency::MutationClaimGuard::acquire_ephemeral(vec![released_claim])
            .expect("WRITE claim must release after editor exit");
    }

    #[cfg(unix)]
    #[test]
    fn foreground_editor_restores_terminal_after_nonzero_and_spawn_failure() {
        use std::os::unix::fs::PermissionsExt;
        use std::sync::atomic::Ordering;

        let _coordination = crate::concurrency::scoped_test_coordination_root();
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("notes.md");
        std::fs::write(&target, b"notes\n").expect("write target");

        let nonzero = dir.path().join("nonzero.sh");
        executable_test_script(&nonzero, "#!/bin/sh\nexit 7\n");
        let state = std::sync::Arc::new(FakeTerminalState::default());
        state.raw_mode.store(true, Ordering::SeqCst);
        state.alternate_screen.store(true, Ordering::SeqCst);
        let result = open_in_editor_with_terminal(
            &target,
            &nonzero.to_string_lossy(),
            FakeTerminalBackend(state.clone()),
        );
        assert_eq!(result, Ok(false));
        assert!(state.raw_mode.load(Ordering::SeqCst));
        assert!(state.alternate_screen.load(Ordering::SeqCst));

        let unexecutable = dir.path().join("unexecutable.sh");
        std::fs::write(&unexecutable, "#!/bin/sh\nexit 0\n").expect("write unexecutable");
        let mut permissions = std::fs::metadata(&unexecutable)
            .expect("stat unexecutable")
            .permissions();
        permissions.set_mode(0o644);
        std::fs::set_permissions(&unexecutable, permissions).expect("chmod unexecutable");

        let spawn_state = std::sync::Arc::new(FakeTerminalState::default());
        spawn_state.raw_mode.store(true, Ordering::SeqCst);
        spawn_state.alternate_screen.store(true, Ordering::SeqCst);
        let error = open_in_editor_with_terminal(
            &target,
            &unexecutable.to_string_lossy(),
            FakeTerminalBackend(spawn_state.clone()),
        )
        .expect_err("non-executable regular file must fail at spawn");
        assert!(error.contains("failed to run"), "unexpected error: {error}");
        assert!(spawn_state.raw_mode.load(Ordering::SeqCst));
        assert!(spawn_state.alternate_screen.load(Ordering::SeqCst));
    }

    #[test]
    fn log_viewer_default_prefers_bat_and_editor_policy_bypasses_it() {
        assert_eq!(
            preferred_log_viewer_command(crate::config::LogViewer::default()),
            Some("bat")
        );
        assert_eq!(
            preferred_log_viewer_command(crate::config::LogViewer::Editor),
            None,
            "the editor opt-out must bypass bat"
        );
    }

    #[test]
    fn missing_bat_launch_falls_back_to_existing_read_only_viewer_path() {
        let path = std::path::Path::new("session.log");
        let mut launch_calls = 0usize;
        let mut fallback_calls = 0usize;
        let result = open_log_viewer_with(
            path,
            crate::config::LogViewer::Bat,
            |actual, command| {
                launch_calls += 1;
                assert_eq!(actual, path);
                assert_eq!(command, "bat");
                Err("executable not found".to_string())
            },
            |actual| {
                fallback_calls += 1;
                assert_eq!(actual, path);
                Ok(true)
            },
        )
        .expect("missing bat must use the normal viewer fallback");
        assert!(result);
        assert_eq!(launch_calls, 1);
        assert_eq!(fallback_calls, 1);
    }

    #[test]
    fn tui_open_in_editor_callers_force_full_redraw_after_every_return_path() {
        let command = include_str!("command.rs");
        let edit_file_arm = command
            .split("Command::EditFile(path) => {")
            .nth(1)
            .expect("edit-file command arm")
            .split("Command::ContextMenu => {")
            .next()
            .expect("edit-file arm body");
        assert!(edit_file_arm.contains("open_in_editor(&target)"));
        assert!(
            edit_file_arm.contains("Ok(_) => {
                    app.force_redraw = true;")
                && edit_file_arm.contains("Err(e) => {
                    app.force_redraw = true;"),
            ":edit-file must force a full redraw after both successful and failed system-editor returns"
        );

        let keybindings = include_str!("keybindings.rs");
        let embedded_call_site = keybindings
            .split("MetadataCuePillAction::Edit => {")
            .nth(1)
            .expect("embedded CUESHEET edit action")
            .split("MetadataCuePillAction::Delete => {")
            .next()
            .expect("embedded CUESHEET edit action body");
        assert!(embedded_call_site.contains("metadata_editor_edit_embedded_cuesheet_with_system_editor"));
        assert!(
            embedded_call_site.contains("app.force_redraw = true;"),
            ":cuesheet-edit must force a full redraw after the external editor returns with accept, reject, unchanged, or error"
        );
    }
}
