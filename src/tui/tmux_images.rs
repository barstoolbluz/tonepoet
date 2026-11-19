//! Query tmux's *control plane*, never the pane's terminal input stream.
//!
//! A Kitty escape probe inside tmux can leak replies into the application's
//! keyboard stream (4467ecc4). Native images are permitted only if the exact
//! pane has passthrough enabled and every client attached to its session is a
//! known Kitty-capable terminal with the RGB feature. Unknown means halfblocks.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct KittySupport {
    pub cell_size: Option<(u16, u16)>,
}

/// Only a verified current pane is admissible; do not substitute the active
/// pane from an unrelated tmux session when TMUX_PANE was not forwarded.
pub(super) fn inspect() -> Option<KittySupport> {
    let pane = std::env::var("TMUX_PANE").ok()?;
    if !valid_pane_id(&pane) { return None; }
    let passthrough = tmux_output(&["show-options", "-p", "-v", "-t", &pane, "allow-passthrough"])?;
    if !passthrough_allowed(&passthrough) { return None; }
    let session = tmux_output(&["display-message", "-p", "-t", &pane, "#{session_name}"])?;
    let session = session.trim();
    if session.is_empty() || session.bytes().any(|byte| matches!(byte, b'\r' | b'\n' | b'\t')) {
        return None;
    }
    let clients = tmux_output(&[
        "list-clients", "-F",
        "#{client_session}\t#{client_termname}\t#{client_termfeatures}\t#{client_cell_width}\t#{client_cell_height}",
    ])?;
    assess_clients(session, &clients)
}

fn valid_pane_id(pane: &str) -> bool {
    let Some(digits) = pane.strip_prefix('%') else { return false; };
    !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
}

fn passthrough_allowed(value: &str) -> bool {
    matches!(value.trim(), "on" | "all")
}

/// Parse every currently attached client of the target session. If even one
/// attached client cannot draw the image, Kitty would be unsafe for the pane.
fn assess_clients(session: &str, clients: &str) -> Option<KittySupport> {
    let mut found = false;
    let mut client_count = 0_usize;
    let mut unknown_size = false;
    let mut cell_size = None;
    for line in clients.lines() {
        let fields: Vec<_> = line.split('\t').collect();
        if fields.len() != 5 { return None; }
        if fields[0] != session { continue; }
        found = true;
        client_count += 1;
        let terminal = fields[1].to_ascii_lowercase();
        // Do not infer outer Kitty support from a tmux pane's own TERM or
        // TERM_PROGRAM. Only recognize the actual attaching client name.
        if !matches!(terminal.as_str(), "xterm-kitty" | "xterm-ghostty" | "kitty" | "ghostty") {
            return None;
        }
        if !fields[2].split(',').any(|feature| feature.trim() == "RGB") {
            return None;
        }
        let (Ok(width), Ok(height)) = (fields[3].parse::<u16>(), fields[4].parse::<u16>())
        else { return None; };
        if width > 256 || height > 512 { return None; }
        if width == 0 || height == 0 {
            // PTYs over SSH often expose zero pixel dimensions. With one
            // client a separate reliable cell-size source can still suffice;
            // with multiple clients an unknown geometry is not proof that
            // the same image placement works for each of them.
            unknown_size = true;
            continue;
        }
        let size = (width, height);
        if cell_size.is_some_and(|previous| previous != size) {
            // Different font geometries cannot share one Kitty placement.
            return None;
        }
        cell_size = Some(size);
    }
    if unknown_size && client_count > 1 { return None; }
    found.then_some(KittySupport { cell_size })
}

/// All tmux invocations have closed standard input and error, and bounded
/// execution time and output size. Neither protocol probes nor escape replies
/// ever traverse stdin/stdout of the pane.
fn tmux_output(args: &[&str]) -> Option<String> {
    let mut child = Command::new("tmux")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn().ok()?;
    let deadline = Instant::now() + Duration::from_millis(250);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    if !status.success() { return None; }
    let mut output = Vec::new();
    child.stdout.take()?.take(64 * 1024 + 1).read_to_end(&mut output).ok()?;
    if output.len() > 64 * 1024 { return None; }
    String::from_utf8(output).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_byobu_ghostty_with_passthrough_rgb_is_admitted() {
        let clients = "session\txterm-ghostty\tRGB,extkeys\t9\t18\n";
        assert_eq!(assess_clients("session", clients), Some(KittySupport { cell_size: Some((9, 18)) }));
        assert_eq!(assess_clients("other", clients), None);
    }

    #[test]
    fn one_incompatible_client_disqualifies_the_entire_session() {
        let good = "demo\txterm-ghostty\tRGB\t10\t20\n";
        for extra in [
            "demo\txterm-256color\tRGB\t10\t20\n",
            "demo\txterm-kitty\t256\t10\t20\n",
            "demo\tunknown\tRGB\t10\t20\n",
            "demo\txterm-kitty\tRGB\t12\t24\n",
            "demo\txterm-kitty\tRGB\t999\t20\n",
            "demo\txterm-kitty\tRGB\tbad\t20\n",
            "incomplete\n",
        ] {
            assert_eq!(assess_clients("demo", &format!("{good}{extra}")), None, "{extra:?}");
        }
    }

    #[test]
    fn passthrough_or_pane_identity_not_proven_must_fall_back() {
        assert!(valid_pane_id("%2"));
        for invalid in ["", "%", "1", "%2;kill-server", "%abc", "%-1"] {
            assert!(!valid_pane_id(invalid), "{invalid:?}");
        }
        assert!(passthrough_allowed("on\n"));
        assert!(passthrough_allowed("all\n"));
        for unsupported in ["off", "", "on\noff", "true", "1"] {
            assert!(!passthrough_allowed(unsupported));
        }
    }

    #[test]
    fn zero_cell_metrics_cannot_be_used_to_claim_geometry() {
        assert_eq!(assess_clients("demo", "demo\txterm-kitty\tRGB\t0\t0\n"),
            Some(KittySupport { cell_size: None }));
        assert_eq!(assess_clients("demo", "demo\txterm-kitty\tRGB\t0\t0\nother\txterm-256color\t0\t0\n"),
            Some(KittySupport { cell_size: None }));
        assert_eq!(assess_clients("demo", "demo\txterm-kitty\tRGB\t0\t0\ndemo\txterm-ghostty\tRGB\t10\t20\n"),
            None, "unknown geometry on any one of several clients is not proof");
    }
}
