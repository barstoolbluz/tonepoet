//! Query tmux's control plane on a worker, never the pane's input stream.
//!
//! A graphics probe through tmux can leak replies into crossterm input. Use
//! tmux's attached-client features and the pane's effective passthrough option
//! instead. Every attached client must agree on a protocol and font geometry;
//! unknown or incompatible states fall back to Unicode halfblocks.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum NativeProtocol {
    Kitty,
    Sixel,
}

/// A tmux window's cell size is not proof of the attached terminal's pixels.
/// tmux 3.5a uses a built-in 16x32 default when its client reports 0x0.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CellGeometry {
    ClientReported((u16, u16)),
    WindowEstimate((u16, u16)),
    Unknown,
}

impl CellGeometry {
    fn observed(self) -> Option<(u16, u16)> {
        match self {
            Self::ClientReported(size) => Some(size),
            Self::WindowEstimate(_) | Self::Unknown => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ImageSupport {
    pub protocol: NativeProtocol,
    pub cell_geometry: CellGeometry,
    // Native image data belongs to the attached emulator, not the tmux
    // session. Any attachment/replacement invalidates cached protocols.
    client_ttys: Vec<String>,
}

impl ImageSupport {
    /// Only measurements reported by the actual attached client are fit for
    /// raster-to-pane placement. A window-level estimate is not authoritative.
    pub(super) fn observed_cell_size(&self) -> Option<(u16, u16)> {
        self.cell_geometry.observed()
    }
}

#[cfg(test)]
impl ImageSupport {
    pub(super) fn test_support(protocol: NativeProtocol, cell_geometry: CellGeometry) -> Self {
        Self { protocol, cell_geometry, client_ttys: vec!["/dev/pts/11".into()] }
    }
}

/// A verified current pane is required; do not substitute an unrelated
/// session's active pane when TMUX_PANE is missing.
/// This function performs subprocess I/O and MUST run off the UI thread.
pub(super) fn inspect() -> Option<ImageSupport> {
    let pane = std::env::var("TMUX_PANE").ok()?;
    if !valid_pane_id(&pane) { return None; }
    // -A resolves pane, window, and global inheritance exactly as tmux uses
    // it when forwarding DCS passthrough from this pane (tmux 3.5a).
    let passthrough = tmux_output(&[
        "show-options", "-p", "-A", "-v", "-t", &pane, "allow-passthrough",
    ])?;
    if !passthrough_allowed(&passthrough) || !pane_can_forward(&pane, &passthrough) {
        return None;
    }
    // SSH commonly reports 0x0 client pixels. tmux retains a window-level
    // size, but this may simply be its built-in 16x32 default rather than a
    // real measurement. Retain its provenance through to picker selection.
    let geometry = tmux_output(&[
        "display-message", "-p", "-t", &pane,
        "#{session_name}\t#{window_cell_width}\t#{window_cell_height}",
    ])?;
    let (session, window_cell_size) = parse_window_geometry(&geometry)?;
    let clients = tmux_output(&[
        "list-clients", "-F",
        "#{client_session}\t#{client_termname}\t#{client_termfeatures}\t#{client_cell_width}\t#{client_cell_height}\t#{client_tty}",
    ])?;
    assess_clients(session, &clients, window_cell_size)
}

fn parse_cell_size(width: &str, height: &str) -> Option<(u16, u16)> {
    let width = width.parse::<u16>().ok()?;
    let height = height.parse::<u16>().ok()?;
    // Zero is unknown. These generous limits reject malformed outputs,
    // not ordinary font customizations.
    (width > 0 && height > 0 && width <= 256 && height <= 512)
        .then_some((width, height))
}

fn parse_window_geometry(output: &str) -> Option<(&str, Option<(u16, u16)>)> {
    let fields: Vec<_> = output.trim_end_matches('\n').split('\t').collect();
    if fields.len() != 3 { return None; }
    let session = fields[0];
    if session.is_empty() || session.chars().any(char::is_control) { return None; }
    Some((session, parse_cell_size(fields[1], fields[2])))
}

fn valid_pane_id(pane: &str) -> bool {
    let Some(digits) = pane.strip_prefix('%') else { return false; };
    !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
}

const PASSTHROUGH_OPTION: &str = "allow-passthrough";

fn passthrough_allowed(value: &str) -> bool {
    matches!(value.trim(), "on" | "all")
}

/// `on` forwards only to visible panes. A window is visible when at least
/// one client views it; a zoomed window displays only its active pane.
fn pane_is_visible(status: &str) -> bool {
    let fields: Vec<_> = status.trim_end_matches('\n').split('\t').collect();
    if fields.len() != 3 { return false; }
    let Ok(viewers) = fields[0].parse::<usize>() else { return false; };
    viewers > 0 && matches!((fields[1], fields[2]), ("0", "0" | "1") | ("1", "1"))
}

fn pane_can_forward(pane: &str, passthrough: &str) -> bool {
    if !passthrough_allowed(passthrough) { return false; }
    // Even `all` would forward raw graphics from an invisible window to
    // whatever the client currently displays. Never leak an image from a
    // hidden pane, regardless of tmux's permissive transport option.
    tmux_output(&[
        "display-message", "-p", "-t", pane,
        "#{window_active_clients}\t#{window_zoomed_flag}\t#{pane_active}",
    ]).is_some_and(|state| pane_is_visible(&state))
}

/// Choose the same native protocol for *every* attached client. The features
/// are supplied by tmux, not the inner pane's TERM_PROGRAM. Kitty requires a
/// known outer terminal plus RGB; Sixel uses tmux's explicit `sixel` feature
/// and does not require its unrelated true-color text feature.
fn assess_clients(
    session: &str, clients: &str, window_cell_size: Option<(u16, u16)>,
) -> Option<ImageSupport> {
    let mut count = 0_usize;
    let mut all_kitty = true;
    let mut all_sixel = true;
    let mut unknown_size = false;
    let mut cell_size = None;
    let mut client_ttys = Vec::new();
    for line in clients.lines() {
        let fields: Vec<_> = line.split('\t').collect();
        if fields.len() != 6 { return None; }
        if fields[0] != session { continue; }
        count += 1;
        if fields[5].is_empty() || fields[5].chars().any(char::is_control) {
            return None;
        }
        client_ttys.push(fields[5].to_owned());
        let terminal = fields[1].to_ascii_lowercase();
        let kitty_name = matches!(terminal.as_str(),
            "xterm-kitty" | "xterm-ghostty" | "kitty" | "ghostty");
        let mut has_rgb = false;
        let mut has_sixel = false;
        for feature in fields[2].split(',') {
            has_rgb |= feature.eq_ignore_ascii_case("RGB");
            has_sixel |= feature.eq_ignore_ascii_case("sixel");
        }
        all_kitty &= kitty_name && has_rgb;
        all_sixel &= has_sixel;
        if !all_kitty && !all_sixel { return None; }

        // Missing/zero client pixel metrics are common over SSH. Window
        // metrics are only an estimate, even with one attached client;
        // multiple unknown clients cannot prove compatible geometry.
        let size = match parse_cell_size(fields[3], fields[4]) {
            Some(size) => size,
            None if fields[3].parse::<u16>().is_ok() &&
                    fields[4].parse::<u16>().is_ok() &&
                    (fields[3] == "0" || fields[4] == "0") => {
                unknown_size = true;
                continue;
            }
            None => return None,
        };
        if cell_size.is_some_and(|previous| previous != size) { return None; }
        cell_size = Some(size);
    }
    if count == 0 || (unknown_size && count > 1) { return None; }
    // Keep window metrics for diagnostics and change detection, but never
    // promote them to measured outer-terminal pixels. With 0x0 client cells,
    // even a live window value can be tmux's 16x32 built-in default.
    let cell_geometry = if unknown_size {
        window_cell_size.map(CellGeometry::WindowEstimate).unwrap_or(CellGeometry::Unknown)
    } else {
        cell_size.map(CellGeometry::ClientReported).unwrap_or(CellGeometry::Unknown)
    };
    client_ttys.sort_unstable();
    if client_ttys.windows(2).any(|pair| pair[0] == pair[1]) { return None; }
    let protocol = if all_kitty {
        NativeProtocol::Kitty
    } else if all_sixel {
        NativeProtocol::Sixel
    } else {
        return None;
    };
    Some(ImageSupport { protocol, cell_geometry, client_ttys })
}

/// All subprocesses close stdin and stderr, have a short deadline and bounded
/// output. This runs only on the image-inspection worker, never on render/input.
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

    fn assess_clients(session: &str, clients: &str) -> Option<ImageSupport> {
        super::assess_clients(session, clients, Some((16, 32)))
    }

    fn ghostty() -> &'static str {
        "session\txterm-ghostty\tRGB,extkeys\t9\t18\t/dev/pts/11\n"
    }

    #[test]
    fn ssh_byobu_ghostty_with_passthrough_rgb_is_admitted() {
        assert_eq!(assess_clients("session", ghostty()), Some(ImageSupport {
            protocol: NativeProtocol::Kitty, cell_geometry: CellGeometry::ClientReported((9, 18)),
            client_ttys: vec!["/dev/pts/11".into()],
        }));
        assert_eq!(assess_clients("other", ghostty()), None);
    }

    #[test]
    fn windows_terminal_sixel_is_admitted_without_rgb_or_kitty() {
        let windows = "session\txterm-256color\tbpaste,clipboard,sixel,title\t9\t18\t/dev/pts/12\n";
        assert_eq!(assess_clients("session", windows), Some(ImageSupport {
            protocol: NativeProtocol::Sixel, cell_geometry: CellGeometry::ClientReported((9, 18)),
            client_ttys: vec!["/dev/pts/12".into()],
        }));
        assert_eq!(assess_clients("session", "session\txterm-256color\tRGB,title\t9\t18\t/dev/pts/12\n"), None);
    }

    #[test]
    fn client_identity_changes_invalidate_identical_geometry_without_reordering_churn() {
        let a = ghostty();
        let b = "session\txterm-ghostty\tRGB\t9\t18\t/dev/pts/12\n";
        let solo = assess_clients("session", a).unwrap();
        let dual = assess_clients("session", &format!("{a}{b}")).unwrap();
        let reordered = assess_clients("session", &format!("{b}{a}")).unwrap();
        let replacement = assess_clients("session", b).unwrap();
        assert_ne!(solo, dual);
        assert_ne!(solo, replacement);
        assert_eq!(dual, reordered);
        assert_eq!(solo.cell_geometry, dual.cell_geometry);
    }

    #[test]
    fn incompatible_clients_must_use_halfblocks_including_kitty_sixel_mix() {
        let sixel = "session\txterm-256color\tsixel\t9\t18\t/dev/pts/12\n";
        assert_eq!(assess_clients("session", &format!("{}{sixel}", ghostty())), None);
        let sixel2 = "session\txterm-256color\tsixel\t9\t18\t/dev/pts/13\n";
        assert_eq!(assess_clients("session", &format!("{sixel}{sixel2}")).unwrap().protocol,
            NativeProtocol::Sixel);
        for extra in [
            "session\txterm-256color\tRGB\t9\t18\t/dev/pts/12\n",
            "session\txterm-ghostty\tRGB\t12\t24\t/dev/pts/12\n",
            "session\txterm-ghostty\tRGB\t999\t20\t/dev/pts/12\n",
            "session\txterm-ghostty\tRGB\tbad\t20\t/dev/pts/12\n",
            "session\txterm-ghostty\tRGB\t9\t18\t\n",
            "session\txterm-ghostty\tRGB\t9\t18\t/dev/pts/11\n",
            "incomplete\n",
        ] {
            assert_eq!(assess_clients("session", &format!("{}{extra}", ghostty())), None,
                "{extra:?}");
        }
    }

    #[test]
    fn passthrough_inherits_and_pane_identity_is_checked() {
        assert!(valid_pane_id("%2"));
        assert!(!valid_pane_id("%2;kill-server"));
        assert!(passthrough_allowed("on\n"));
        assert!(passthrough_allowed("all\n"));
        assert!(passthrough_allowed("all"));
        assert!(!pane_can_forward("%2", "off"));
        assert_eq!(PASSTHROUGH_OPTION, "allow-passthrough");
        for unsupported in ["off", "", "on\noff", "true", "1"] {
            assert!(!passthrough_allowed(unsupported));
        }
    }

    #[test]
    fn hidden_window_or_obscured_zoomed_pane_cannot_forward_on_mode() {
        assert!(pane_is_visible("1\t0\t0\n"));
        assert!(pane_is_visible("2\t1\t1\n"));
        // `allow-passthrough all` must not turn a hidden image into a
        // cross-window upload, even though tmux would transport its DCS.
        assert!(passthrough_allowed("all"));
        assert!(!pane_is_visible("0\t0\t1\n"));
        for hidden in ["0\t0\t1\n", "0\t1\t1\n", "2\t1\t0\n", "garbage", "1\t\t1\n"] {
            assert!(!pane_is_visible(hidden));
        }
    }

    #[test]
    fn unknown_client_cells_keep_window_metrics_as_unverified_estimates() {
        let windows = "demo\txterm-256color\tsixel\t0\t0\t/dev/pts/12\n";
        assert_eq!(super::assess_clients("demo", windows, Some((11, 23))),
            Some(ImageSupport { protocol: NativeProtocol::Sixel,
                cell_geometry: CellGeometry::WindowEstimate((11, 23)),
                client_ttys: vec!["/dev/pts/12".into()] }));
        let fallback = super::assess_clients("demo", windows, Some((17, 31))).unwrap();
        assert_eq!(fallback.cell_geometry, CellGeometry::WindowEstimate((17, 31)));
        assert_eq!(fallback.observed_cell_size(), None);
        let missing = super::assess_clients("demo", windows, None).unwrap();
        assert_eq!(missing.cell_geometry, CellGeometry::Unknown);
        assert_eq!(missing.observed_cell_size(), None);
        // A second unknown-size client could have a different customized
        // font even when tmux reports a single window fallback.
        assert_eq!(super::assess_clients("demo",
            &format!("{windows}demo\txterm-256color\tsixel\t0\t0\t/dev/pts/13\n"),
            Some((16, 32))), None);
        assert_eq!(parse_window_geometry("demo\t9\t19\n"), Some(("demo", Some((9,19)))));
        assert_eq!(parse_window_geometry("demo\t0\t0\n"), Some(("demo", None)));
    }

    #[test]
    fn zero_cell_metrics_cannot_be_used_to_claim_geometry() {
        assert_eq!(assess_clients("demo", "demo\txterm-kitty\tRGB\t0\t0\t/dev/pts/11\n"),
            Some(ImageSupport { protocol: NativeProtocol::Kitty,
                cell_geometry: CellGeometry::WindowEstimate((16, 32)),
                client_ttys: vec!["/dev/pts/11".into()] }));
        // A different session must still have the full tmux row shape.
        assert_eq!(assess_clients("demo", "demo\txterm-kitty\tRGB\t0\t0\t/dev/pts/11\nother\txterm-256color\tsixel\t0\t0\t/dev/pts/12\n"),
            Some(ImageSupport { protocol: NativeProtocol::Kitty,
                cell_geometry: CellGeometry::WindowEstimate((16, 32)),
                client_ttys: vec!["/dev/pts/11".into()] }));
        assert_eq!(assess_clients("demo", "demo\txterm-kitty\tRGB\t0\t0\t/dev/pts/11\ndemo\txterm-ghostty\tRGB\t10\t20\t/dev/pts/12\n"),
            None);
    }
}
