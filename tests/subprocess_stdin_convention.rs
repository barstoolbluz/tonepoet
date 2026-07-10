//! Sentinel: every subprocess launched via `.spawn()` or `.status()` must
//! configure stdin explicitly (normally `Stdio::null()`), because those two
//! launchers INHERIT stdin by default and a tool that decides to prompt
//! ("Overwrite? [y/N]", license nags, wrapper scripts) wedges the TUI or a
//! background probe forever — the archive-extraction hang and the version-
//! probe wedge were both this class. `Command::output()` is exempt: Rust
//! nulls stdin there by default.
//!
//! The ONE deliberate exemption is the external editor spawn, which must
//! inherit the user's terminal; it is annotated at the site.

use std::path::Path;

fn scan_dir(dir: &Path, violations: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).expect("read src dir").flatten() {
        let path = entry.path();
        if path.is_dir() {
            scan_dir(&path, violations);
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let source = std::fs::read_to_string(&path).expect("read source file");
        let mut search_from = 0;
        while let Some(rel) = source[search_from..].find("Command::new(") {
            let start = search_from + rel;
            // A launch chain ends at the first `;` — bound the window there.
            let window_end = source[start..]
                .find(';')
                .map(|i| start + i)
                .unwrap_or(source.len());
            let chain = &source[start..window_end];
            search_from = start + "Command::new(".len();

            let launches_inheriting = chain.contains(".spawn()") || chain.contains(".status()");
            if !launches_inheriting || chain.contains(".stdin(") {
                continue;
            }
            // The documented exemption: the $EDITOR spawn inherits the TTY.
            if path.ends_with("src/tui/external_editor.rs")
                && chain.contains("Command::new(program)")
            {
                continue;
            }
            let line = source[..start].matches('\n').count() + 1;
            violations.push(format!("{}:{}", path.display(), line));
        }
    }
}

#[test]
fn spawn_and_status_subprocesses_configure_stdin() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut violations = Vec::new();
    scan_dir(&src, &mut violations);
    assert!(
        violations.is_empty(),
        "subprocess launches inheriting stdin (add .stdin(Stdio::null()) or document an exemption here):\n{}",
        violations.join("\n")
    );
}
