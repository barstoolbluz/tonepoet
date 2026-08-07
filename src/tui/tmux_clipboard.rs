//! Opt-in tmux/byobu OSC 52 clipboard configuration.
//!
//! When `[ui] manage_tmux_clipboard = true` in config.toml, TUI startup
//! ensures the user's tmux configuration contains a marker-delimited block
//! enabling OSC 52 clipboard passthrough, so copies from tonepoet (see
//! `host_clipboard`) reach the system clipboard through the outer terminal.
//!
//! Responsibility contract (this touches a dotfile we do not own):
//! - Strictly opt-in; the flag defaults to off and is never set by tonepoet.
//! - Idempotent: all writes live between `# >>> tonepoet clipboard >>>` /
//!   `# <<< tonepoet clipboard <<<` markers; nothing outside the block is
//!   ever modified, and an unchanged block is never rewritten.
//! - The previous file is backed up to `<name>.tonepoet-clipboard.bak`
//!   before the first modification (a name distinct from the metadata
//!   `.tonepoet-bak` convention so recovery scanners never consider it).
//! - Byobu's tmux backend reads `~/.byobu/.tmux.conf`, not `~/.tmux.conf`;
//!   the target is chosen from `$BYOBU_BACKEND` / `$TMUX`. Outside any tmux
//!   session (or under byobu's screen backend) nothing is touched.
//!
//! TODO(config-screen): expose `manage_tmux_clipboard` as an explicit toggle
//! in the new/improved Config screen once that screen is built out. Until
//! then it is reachable only by editing config.toml directly. See memory
//! `tmux_clipboard_config_exposure`.

use std::path::{Path, PathBuf};

pub const MARKER_BEGIN: &str = "# >>> tonepoet clipboard >>>";
pub const MARKER_END: &str = "# <<< tonepoet clipboard <<<";

/// Outcome of an idempotent block write.
#[derive(Debug, PartialEq, Eq)]
pub enum EnsureOutcome {
    /// The block already exists with the desired content; nothing written.
    AlreadyCurrent,
    /// The file did not exist and was created with the block.
    Created,
    /// An existing file was updated; the previous version was backed up
    /// to the contained path first.
    Updated { backup: PathBuf },
}

/// Pick the tmux config file that the current multiplexer actually loads.
///
/// Pure function over the relevant environment so tests can cover every
/// branch without mutating process env.
pub fn target_config_path_from(
    byobu_backend: Option<&str>,
    tmux_env_set: bool,
    home: &Path,
) -> Option<PathBuf> {
    match byobu_backend {
        // Byobu's tmux profile sources ~/.byobu/.tmux.conf and ignores
        // ~/.tmux.conf entirely.
        Some("tmux") => Some(home.join(".byobu").join(".tmux.conf")),
        // Byobu on the screen backend: a tmux config would change nothing.
        Some(_) => None,
        None if tmux_env_set => Some(home.join(".tmux.conf")),
        None => None,
    }
}

fn target_config_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let byobu_backend = std::env::var("BYOBU_BACKEND").ok();
    target_config_path_from(
        byobu_backend.as_deref(),
        std::env::var_os("TMUX").is_some(),
        &home,
    )
}

/// Parse "tmux 3.5a" / "tmux next-3.6" style version strings into
/// (major, minor). Returns None when no digits are present.
pub fn parse_tmux_version(output: &str) -> Option<(u32, u32)> {
    let digits_start = output.find(|c: char| c.is_ascii_digit())?;
    let rest = &output[digits_start..];
    let mut parts = rest.split('.');
    let major: u32 = parts
        .next()?
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse()
        .ok()?;
    let minor: u32 = parts
        .next()
        .map(|m| {
            m.chars()
                .take_while(|c| c.is_ascii_digit())
                .collect::<String>()
        })
        .and_then(|m| m.parse().ok())
        .unwrap_or(0);
    Some((major, minor))
}

fn detected_tmux_version() -> Option<(u32, u32)> {
    let output = std::process::Command::new("tmux").arg("-V").output().ok()?;
    parse_tmux_version(&String::from_utf8_lossy(&output.stdout))
}

/// Render the managed block for a given tmux version. Unknown versions get
/// the modern (>= 3.3) form: any tmux new enough to matter in 2026 accepts
/// it, and older servers ignore unknown options with a warning rather than
/// failing to start.
pub fn desired_block(version: Option<(u32, u32)>) -> String {
    let (major, minor) = version.unwrap_or((3, 3));
    let mut lines = vec![
        MARKER_BEGIN.to_string(),
        "# Managed by tonepoet ([ui] manage_tmux_clipboard). Edits inside this".to_string(),
        "# block are overwritten; remove the block or disable the setting to".to_string(),
        "# opt out. Enables OSC 52 clipboard passthrough.".to_string(),
        "set -g set-clipboard on".to_string(),
    ];
    if (major, minor) >= (3, 2) {
        lines.push("set -as terminal-features ',*:clipboard'".to_string());
    } else {
        lines.push(
            "set -as terminal-overrides ',*:Ms=\\E]52;%p1%s;%p2%s\\007'".to_string(),
        );
    }
    if (major, minor) >= (3, 3) {
        lines.push("set -g allow-passthrough on".to_string());
    }
    lines.push(MARKER_END.to_string());
    lines.join("\n")
}

/// Splice the desired block into existing file contents: replace an existing
/// marker span in place, otherwise append separated by a blank line. Content
/// outside the markers is preserved byte-for-byte.
pub fn splice_block(existing: &str, block: &str) -> String {
    if let (Some(begin), Some(end_start)) = (existing.find(MARKER_BEGIN), existing.find(MARKER_END))
    {
        if end_start >= begin {
            let end = end_start + MARKER_END.len();
            let mut out = String::with_capacity(existing.len() + block.len());
            out.push_str(&existing[..begin]);
            out.push_str(block);
            out.push_str(&existing[end..]);
            return out;
        }
    }
    let mut out = existing.to_string();
    if !out.is_empty() {
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }
    out.push_str(block);
    out.push('\n');
    out
}

/// Idempotently ensure `path` contains the desired managed block.
pub fn ensure_clipboard_block(
    path: &Path,
    version: Option<(u32, u32)>,
) -> std::io::Result<EnsureOutcome> {
    let block = desired_block(version);
    let existing = match std::fs::read(path) {
        Ok(bytes) => Some(String::from_utf8(bytes).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "existing config is not valid UTF-8; refusing to rewrite it",
            )
        })?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };

    let updated = splice_block(existing.as_deref().unwrap_or(""), &block);
    if existing.as_deref() == Some(updated.as_str()) {
        return Ok(EnsureOutcome::AlreadyCurrent);
    }

    let backup = existing.is_some().then(|| {
        let mut name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| ".tmux.conf".to_string());
        name.push_str(".tonepoet-clipboard.bak");
        path.with_file_name(name)
    });
    if let (Some(backup), Some(existing)) = (backup.as_ref(), existing.as_ref()) {
        std::fs::write(backup, existing)?;
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Write-then-rename so a crash never leaves a truncated multiplexer config.
    let tmp = path.with_extension("tonepoet-clipboard.tmp");
    std::fs::write(&tmp, &updated)?;
    std::fs::rename(&tmp, path)?;

    Ok(match backup {
        Some(backup) => EnsureOutcome::Updated { backup },
        None => EnsureOutcome::Created,
    })
}

/// Startup entry point: apply the managed block when the config flag is on
/// and we are actually inside a tmux-backed multiplexer. Returns a status
/// message for state changes and failures; silent (None) when disabled,
/// not applicable, or already current.
pub fn apply_if_enabled(config: &crate::config::TonepoetConfig) -> Option<String> {
    if !config.ui.manage_tmux_clipboard {
        return None;
    }
    let Some(path) = target_config_path() else {
        log::debug!("manage_tmux_clipboard: not inside a tmux-backed session; skipping");
        return None;
    };
    match ensure_clipboard_block(&path, detected_tmux_version()) {
        Ok(EnsureOutcome::AlreadyCurrent) => {
            log::debug!("manage_tmux_clipboard: {} already current", path.display());
            None
        }
        Ok(outcome) => {
            // Load the new options into the running server so the change
            // takes effect without a restart. Sourcing the same file the
            // multiplexer itself loads keeps this side-effect-free.
            let _ = std::process::Command::new("tmux")
                .arg("source-file")
                .arg(&path)
                .output();
            Some(match outcome {
                EnsureOutcome::Created => format!(
                    "tmux clipboard: enabled OSC 52 passthrough in new {}",
                    path.display()
                ),
                EnsureOutcome::Updated { backup } => format!(
                    "tmux clipboard: enabled OSC 52 passthrough in {} (backup: {})",
                    path.display(),
                    backup.display()
                ),
                EnsureOutcome::AlreadyCurrent => unreachable!(),
            })
        }
        Err(e) => Some(format!(
            "tmux clipboard: could not update {}: {e}",
            path.display()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byobu_tmux_backend_targets_byobu_conf() {
        let home = Path::new("/home/u");
        assert_eq!(
            target_config_path_from(Some("tmux"), true, home),
            Some(PathBuf::from("/home/u/.byobu/.tmux.conf"))
        );
        // Byobu exports BYOBU_BACKEND even when $TMUX is also set; byobu wins.
        assert_eq!(
            target_config_path_from(Some("tmux"), false, home),
            Some(PathBuf::from("/home/u/.byobu/.tmux.conf"))
        );
    }

    #[test]
    fn byobu_screen_backend_is_not_touched() {
        assert_eq!(
            target_config_path_from(Some("screen"), true, Path::new("/home/u")),
            None
        );
    }

    #[test]
    fn plain_tmux_targets_home_conf_and_no_multiplexer_targets_nothing() {
        let home = Path::new("/home/u");
        assert_eq!(
            target_config_path_from(None, true, home),
            Some(PathBuf::from("/home/u/.tmux.conf"))
        );
        assert_eq!(target_config_path_from(None, false, home), None);
    }

    #[test]
    fn version_parsing_handles_release_and_prerelease_strings() {
        assert_eq!(parse_tmux_version("tmux 3.5a"), Some((3, 5)));
        assert_eq!(parse_tmux_version("tmux 3.2"), Some((3, 2)));
        assert_eq!(parse_tmux_version("tmux next-3.6"), Some((3, 6)));
        assert_eq!(parse_tmux_version("tmux 2.9a"), Some((2, 9)));
        assert_eq!(parse_tmux_version("tmux openbsd-6.9"), Some((6, 9)));
        assert_eq!(parse_tmux_version("garbage"), None);
    }

    #[test]
    fn block_is_version_aware() {
        let modern = desired_block(Some((3, 5)));
        assert!(modern.contains("set -g set-clipboard on"));
        assert!(modern.contains("terminal-features ',*:clipboard'"));
        assert!(modern.contains("allow-passthrough on"));

        let v32 = desired_block(Some((3, 2)));
        assert!(v32.contains("terminal-features"));
        assert!(!v32.contains("allow-passthrough"));

        let legacy = desired_block(Some((2, 9)));
        assert!(legacy.contains("terminal-overrides"));
        assert!(legacy.contains("Ms="));
        assert!(!legacy.contains("terminal-features"));
        assert!(!legacy.contains("allow-passthrough"));

        // Unknown version falls back to the modern form.
        assert_eq!(desired_block(None), desired_block(Some((3, 3))));
    }

    #[test]
    fn splice_appends_preserving_existing_content() {
        let out = splice_block("set -g mouse on", &desired_block(None));
        assert!(out.starts_with("set -g mouse on\n\n# >>> tonepoet clipboard >>>"));
        assert!(out.ends_with("# <<< tonepoet clipboard <<<\n"));
    }

    #[test]
    fn splice_replaces_existing_block_in_place() {
        let original = format!(
            "before\n{}\nold contents\n{}\nafter\n",
            MARKER_BEGIN, MARKER_END
        );
        let out = splice_block(&original, &desired_block(Some((3, 5))));
        assert!(out.starts_with("before\n# >>> tonepoet clipboard >>>"));
        assert!(out.ends_with("# <<< tonepoet clipboard <<<\nafter\n"));
        assert!(!out.contains("old contents"));
        assert_eq!(out.matches(MARKER_BEGIN).count(), 1);
    }

    #[test]
    fn ensure_creates_updates_and_settles_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".byobu").join(".tmux.conf");
        let version = Some((3, 5));

        // Missing file → created, no backup.
        assert_eq!(
            ensure_clipboard_block(&path, version).unwrap(),
            EnsureOutcome::Created
        );
        // Second run is a no-op.
        assert_eq!(
            ensure_clipboard_block(&path, version).unwrap(),
            EnsureOutcome::AlreadyCurrent
        );

        // User content added around the block survives an update triggered
        // by a version change, and the prior file is backed up.
        let mut contents = std::fs::read_to_string(&path).unwrap();
        contents.push_str("\nset -g mouse on\n");
        std::fs::write(&path, &contents).unwrap();
        let outcome = ensure_clipboard_block(&path, Some((3, 2))).unwrap();
        let EnsureOutcome::Updated { backup } = outcome else {
            panic!("expected Updated, got {outcome:?}");
        };
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), contents);
        let updated = std::fs::read_to_string(&path).unwrap();
        assert!(updated.ends_with("set -g mouse on\n"));
        assert!(!updated.contains("allow-passthrough"));
        assert_eq!(updated.matches(MARKER_BEGIN).count(), 1);
    }

    #[test]
    fn ensure_refuses_non_utf8_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".tmux.conf");
        std::fs::write(&path, [0xff, 0xfe, 0x00]).unwrap();
        let err = ensure_clipboard_block(&path, None).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        // File untouched.
        assert_eq!(std::fs::read(&path).unwrap(), vec![0xff, 0xfe, 0x00]);
    }
}
