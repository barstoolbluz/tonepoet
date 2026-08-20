//! Launch an external text editor, suspending the TUI while it runs.

use std::path::Path;
use std::process::Command;

struct TuiRestoreGuard {
    restore: bool,
}

impl TuiRestoreGuard {
    fn suspend() -> Self {
        // Suspend TUI: restore normal terminal mode so the editor can run.
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::event::DisableMouseCapture,
            crossterm::event::DisableBracketedPaste,
            crossterm::cursor::Show,
            crossterm::terminal::LeaveAlternateScreen,
        );
        Self { restore: true }
    }

    fn restore_now(&mut self) {
        if !self.restore {
            return;
        }
        self.restore = false;
        restore_tui_terminal();
    }
}

impl Drop for TuiRestoreGuard {
    fn drop(&mut self) {
        self.restore_now();
    }
}

fn restore_tui_terminal() {
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
    let (program, args) = split_command(&editor_str);

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

    let mut terminal_restore = TuiRestoreGuard::suspend();

    // Run the editor, blocking until it exits. The terminal restore guard
    // above intentionally survives this fallible spawn so early returns
    // cannot leave the TUI suspended. Documented exemption from the
    // subprocess stdin-nulling convention (see the sentinel test
    // tests/subprocess_stdin_convention.rs): the user's $EDITOR needs the
    // terminal — DELIBERATE stdin inheritance.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let status = run_supervised_interactive_editor(program, &args, &admitted_path, mutation_claim)
            .map_err(|e| format!("failed to run {}: {}", editor_str, e))?;
        terminal_restore.restore_now();
        Ok(status.success())
    }

    // Tonepoet's durable process-tree supervisor currently has production
    // backends only on Linux and macOS. Refuse a mutation-capable external
    // editor elsewhere rather than letting a third-party process outlive the
    // only holder of the WRITE claim. The restore guard runs on this return.
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _mutation_claim = mutation_claim;
        Err(format!(
            "failed to run {}: durable external-editor supervision is unavailable on this platform",
            editor_str
        ))
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn run_supervised_interactive_editor(
    program: &str,
    args: &[&str],
    path: &Path,
    mutation_claim: crate::concurrency::MutationClaimGuard,
) -> Result<std::process::ExitStatus, String> {
    use crate::convert::script_supervisor::{
        run_supervised, ContainmentPreference, RuntimeDirectoryIdentity, SupervisedCommand,
    };
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    use std::sync::Arc;

    let binary_path = resolve_editor_program(program)?;
    let binary_file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(&binary_path)
        .map_err(|error| format!("open exact editor executable {}: {error}", binary_path.display()))?;
    if !binary_file
        .metadata()
        .map_err(|error| format!("stat editor executable {}: {error}", binary_path.display()))?
        .is_file()
    {
        return Err(format!("editor executable is not a regular file: {}", binary_path.display()));
    }

    let cwd = std::env::current_dir()
        .and_then(std::fs::canonicalize)
        .map_err(|error| format!("resolve editor working directory: {error}"))?;
    let cwd_file = std::fs::File::open(&cwd)
        .map_err(|error| format!("open editor working directory {}: {error}", cwd.display()))?;

    let runtime_base = std::env::temp_dir().join("tonepoet-interactive-editor-supervisor");
    std::fs::create_dir_all(&runtime_base)
        .map_err(|error| format!("create editor supervisor root: {error}"))?;
    std::fs::set_permissions(&runtime_base, std::fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("protect editor supervisor root: {error}"))?;
    let token = uuid::Uuid::new_v4().simple().to_string();
    let runtime_directory = runtime_base.join(&token);
    std::fs::create_dir(&runtime_directory)
        .map_err(|error| format!("create editor supervisor runtime: {error}"))?;
    std::fs::set_permissions(&runtime_directory, std::fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("protect editor supervisor runtime: {error}"))?;
    let runtime_meta = std::fs::metadata(&runtime_directory)
        .map_err(|error| format!("stat editor supervisor runtime: {error}"))?;

    // Attach the contained editor to the controlling terminal while the TUI is
    // suspended. The tonepoet helper receives these only as ordinary stdio;
    // the mutation lease is a separate supervisor-retained CLOEXEC descriptor.
    let tty = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .map_err(|error| format!("open controlling terminal for editor: {error}"))?;
    let stdin_file = Arc::new(tty.try_clone().map_err(|error| format!("clone editor stdin: {error}"))?);
    let stdout_file = Arc::new(tty.try_clone().map_err(|error| format!("clone editor stdout: {error}"))?);
    let stderr_file = Arc::new(tty);

    let path_arg = path
        .to_str()
        .ok_or_else(|| format!("cannot safely supervise editor for non-UTF-8 path: {}", path.display()))?;
    let mut invocation_args = args.iter().map(|arg| (*arg).to_string()).collect::<Vec<_>>();
    invocation_args.push(path_arg.to_string());
    let lease = Arc::new(mutation_claim.into_lease());
    let retained = lease
        .duplicate_lifetime_file()
        .map_err(|error| format!("duplicate editor mutation lease: {error}"))?;
    let invocation = SupervisedCommand {
        token,
        runtime_directory: runtime_directory.clone(),
        script_file: Arc::new(binary_file),
        working_directory_file: Arc::new(cwd_file),
        script: binary_path,
        args: invocation_args,
        working_directory: cwd,
        environment: std::env::vars().collect(),
        // Interactive editors have no useful application timeout. Process-tree
        // termination still follows cancellation/parent-loss containment.
        timeout: std::time::Duration::from_secs(365 * 24 * 60 * 60),
        runtime_identity: RuntimeDirectoryIdentity {
            device: runtime_meta.dev(),
            inode: runtime_meta.ino(),
        },
        containment_preference: ContainmentPreference::Auto,
        helper_executable: None,
        retained_lifetime_files: vec![retained],
        stdin_file: Some(stdin_file),
        stdout_file: Some(stdout_file),
        stderr_file: Some(stderr_file),
    };

    let outcome = run_supervised(&invocation, || false, |_event| Ok(()))
        .map_err(|error| error.to_string());
    // Keep the parent-side claim holder alive until the helper has settled the
    // complete contained process tree. The helper's duplicate is the authority
    // if this parent dies first.
    drop(lease);
    if outcome.as_ref().is_ok_and(|result| result.containment_empty) {
        let _ = std::fs::remove_dir_all(&runtime_directory);
    }
    outcome.map(|result| result.status)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
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

    #[test]
    fn editor_spawn_error_path_is_guarded_by_terminal_restore_drop() {
        let source = include_str!("external_editor.rs");
        let editor_fn = source
            .split("pub fn open_in_editor")
            .nth(1)
            .expect("open_in_editor function")
            .split("pub fn open_in_viewer")
            .next()
            .expect("open_in_editor body");
        assert!(editor_fn.contains("TuiRestoreGuard::suspend()"));
        assert!(editor_fn.contains("run_supervised_interactive_editor"));
        assert!(editor_fn.contains("durable external-editor supervision is unavailable"));
        assert!(editor_fn.contains("terminal_restore.restore_now();"));

        let viewer_fn = source
            .split("fn run_read_only_command")
            .nth(1)
            .expect("read-only command runner")
            .split("fn preferred_log_viewer_command")
            .next()
            .expect("read-only command runner body");
        assert!(viewer_fn.contains("TuiRestoreGuard::suspend()"));
        assert!(viewer_fn.contains(".map_err(|e| format!(\"failed to run {}: {}\", editor_str, e))?"));
        assert!(viewer_fn.contains("terminal_restore.restore_now();"));
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
