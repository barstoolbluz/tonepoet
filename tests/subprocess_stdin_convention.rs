//! Sentinel: every subprocess launched via `.spawn()` or `.status()` must
//! configure stdin explicitly (normally `Stdio::null()`), because those two
//! launchers INHERIT stdin by default and a tool that decides to prompt
//! ("Overwrite? [y/N]", license nags, wrapper scripts) wedges the TUI or a
//! background probe forever — the archive-extraction hang and the version-
//! probe wedge were both this class. `Command::output()` is exempt: Rust
//! nulls stdin there by default (std and tokio alike).
//!
//! Deliberate exemptions (spawns that MUST inherit the terminal, e.g. the
//! external $EDITOR) are annotated at the site with the literal marker
//! `DELIBERATE stdin inheritance`, which this sentinel honors.
//!
//! Detection covers both launch shapes:
//! 1. single-statement chains: `Command::new(x).args(..).spawn()`
//! 2. builder patterns: `let mut cmd = Command::new(x); cmd.args(..);
//!    cmd.spawn()` — the shape that hid real violations in the streaming
//!    runner and the archive repackager during the first sweep.
//! For each `.spawn()`/`.status()` call, the window back to the nearest
//! preceding `Command::new(` must contain `.stdin(` (or the exemption
//! marker). A window containing `.output()` is skipped — that Command was
//! consumed safely and the launch belongs to something else (e.g. a
//! reqwest response's `.status()`). Known residual blind spot: a Command
//! built in one function and spawned in another (the window is capped at
//! 2000 chars, so cross-function builders fall outside it) — the repackage
//! helper is the one such site and nulls stdin at the spawn.

use std::path::Path;

const EXEMPTION_MARKER: &str = "DELIBERATE stdin inheritance";
const WINDOW_CAP: usize = 2000;

fn scan_file(path: &Path, violations: &mut Vec<String>) {
    let source = std::fs::read_to_string(path).expect("read source file");
    for launcher in [".spawn()", ".status()"] {
        let mut search_from = 0;
        while let Some(rel) = source[search_from..].find(launcher) {
            let launch = search_from + rel;
            search_from = launch + launcher.len();

            // Window: from the nearest preceding `Command::new(` (within the
            // cap) through the launch. No Command in the window means this
            // `.status()`/`.spawn()` belongs to something else entirely
            // (HTTP responses, tokio::spawn-like APIs).
            let window_start = launch.saturating_sub(WINDOW_CAP);
            let Some(cmd_rel) = source[window_start..launch].rfind("Command::new(") else {
                continue;
            };
            let cmd_abs = window_start + cmd_rel;
            let window = &source[cmd_abs..launch];
            // The exemption comment sits just above the Command::new line.
            let marker_window = &source[cmd_abs.saturating_sub(300)..launch];

            if window.contains(".stdin(") || marker_window.contains(EXEMPTION_MARKER) {
                continue;
            }
            // A consumed-by-output() Command is stdin-safe; the launch we
            // matched is then not this Command's.
            if window.contains(".output()") {
                continue;
            }
            let line = source[..launch].matches('\n').count() + 1;
            violations.push(format!("{}:{}", path.display(), line));
        }
    }
}

fn scan_dir(dir: &Path, violations: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).expect("read source dir").flatten() {
        let path = entry.path();
        if path.is_dir() {
            scan_dir(&path, violations);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            scan_file(&path, violations);
        }
    }
}

#[test]
fn spawn_and_status_subprocesses_configure_stdin() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut violations = Vec::new();
    scan_dir(&root.join("src"), &mut violations);
    // Workspace members ship in the same binary and run the same tools.
    // Derive them from Cargo.toml so members outside crates/ (e.g.
    // tonepoet-pipeline at the repo root) stay covered.
    let manifest = std::fs::read_to_string(root.join("Cargo.toml")).expect("read Cargo.toml");
    let members_line = manifest
        .lines()
        .find(|line| line.trim_start().starts_with("members"))
        .expect("workspace members line in Cargo.toml");
    let mut scanned_members = 0;
    for member in members_line.split('"').skip(1).step_by(2) {
        let src = root.join(member).join("src");
        assert!(
            src.is_dir(),
            "workspace member '{member}' has no src/ — sentinel scan roots are stale"
        );
        scan_dir(&src, &mut violations);
        scanned_members += 1;
    }
    assert!(
        scanned_members >= 8,
        "expected to scan all workspace members, got {scanned_members} — members parse is broken"
    );
    assert!(
        violations.is_empty(),
        "subprocess launches inheriting stdin (add .stdin(Stdio::null()) or annotate `DELIBERATE stdin inheritance`):\n{}",
        violations.join("\n")
    );
}
