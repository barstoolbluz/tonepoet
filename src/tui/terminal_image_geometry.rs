//! Terminal character-cell dimensions for terminal graphics over SSH.
//!
//! A remote PTY commonly reports zero *pixel* dimensions through TIOCGWINSZ
//! even though its character rows/columns are correct. XTWINOPS CSI 16 t asks
//! the actual terminal for its character-cell pixel dimensions. Query only
//! after raw/alternate-screen setup and BEFORE starting crossterm's event
//! reader: two consumers must never race to read terminal input.

use std::time::Duration;

const MAX_CELL_WIDTH: u16 = 256;
const MAX_CELL_HEIGHT: u16 = 512;

fn reasonable_cell_size((width, height): (u16, u16)) -> bool {
    width > 0 && height > 0 && width <= MAX_CELL_WIDTH && height <= MAX_CELL_HEIGHT
}

/// Optional escape hatch for terminals that cannot answer XTWINOPS CSI 16 t.
/// The units are pixels per character cell, expressed as `WIDTHxHEIGHT`.
pub fn configured_cell_size() -> Option<(u16, u16)> {
    std::env::var("TONEPOET_IMAGE_CELL_SIZE")
        .ok()
        .and_then(|value| parse_configured_cell_size(&value))
}

fn parse_configured_cell_size(value: &str) -> Option<(u16, u16)> {
    let value = value.trim();
    let (width, height) = value.split_once('x').or_else(|| value.split_once('X'))?;
    let size = (width.parse().ok()?, height.parse().ok()?);
    reasonable_cell_size(size).then_some(size)
}

/// Parse a completed XTWINOPS response, CSI 6 ; height ; width t.
/// Reject malformed, partial, zero, and unreasonable values. Never accept
/// untrusted terminal bytes as an escape sequence to send back to the host.
fn parse_cell_size_response(bytes: &[u8]) -> Option<(u16, u16)> {
    for start in 0..bytes.len().saturating_sub(3) {
        if !bytes[start..].starts_with(b"\x1b[6;") { continue; }
        let suffix = &bytes[start + 4..];
        let Some(end) = suffix.iter().position(|&b| b == b't') else { continue; };
        let report = &suffix[..end];
        let Some(sep) = report.iter().position(|&b| b == b';') else { continue; };
        if report[sep + 1..].contains(&b';') { continue; }
        let parse = |part: &[u8]| -> Option<u16> {
            if part.is_empty() || !part.iter().all(u8::is_ascii_digit) { return None; }
            std::str::from_utf8(part).ok()?.parse().ok()
        };
        let (Some(height), Some(width)) =
            (parse(&report[..sep]), parse(&report[sep + 1..]))
        else { continue; };
        let size = (width, height);
        if reasonable_cell_size(size) { return Some(size); }
    }
    None
}

/// Query once at TUI startup. The timeout is an absolute deadline, not a
/// per-byte timeout, so an unsupported or noisy terminal cannot hang startup.
/// No terminal I/O is attempted in tests, non-TTY sessions, or on Windows.
#[cfg(unix)]
pub fn query_cell_size(timeout: Duration) -> Option<(u16, u16)> {
    use std::io::{IsTerminal, Write};
    use std::os::fd::AsRawFd;
    use std::time::Instant;

    let stdin = std::io::stdin();
    if !stdin.is_terminal() || !std::io::stdout().is_terminal() {
        return None;
    }
    {
        let mut stdout = std::io::stdout().lock();
        stdout.write_all(b"\x1b[16t").ok()?;
        stdout.flush().ok()?;
    }

    let fd = stdin.as_raw_fd();
    let deadline = Instant::now() + timeout;
    let mut response = [0u8; 96];
    let mut len = 0usize;
    while len < response.len() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() { return None; }
        let millis = remaining.as_millis().max(1).min(i32::MAX as u128) as i32;
        let mut poll_fd = libc::pollfd {
            fd, events: libc::POLLIN, revents: 0,
        };
        // SAFETY: poll receives one initialized pollfd; read receives one
        // writable byte from a fixed-size stack buffer. Neither pointer can
        // outlive this synchronous call.
        let ready = unsafe { libc::poll(&mut poll_fd, 1, millis) };
        if ready < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return None;
        }
        if ready == 0 || poll_fd.revents & libc::POLLIN == 0 { return None; }
        let read = unsafe {
            libc::read(fd, (&mut response[len] as *mut u8).cast(), 1)
        };
        if read != 1 { return None; }
        len += 1;
        if let Some(size) = parse_cell_size_response(&response[..len]) {
            return Some(size);
        }
    }
    None
}

#[cfg(not(unix))]
pub fn query_cell_size(_timeout: Duration) -> Option<(u16, u16)> { None }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xtwinops_reports_height_first_and_width_second() {
        assert_eq!(parse_cell_size_response(b"\x1b[6;24;12t"), Some((12, 24)));
        assert_eq!(parse_cell_size_response(b"prefix\x1b[6;19;9t"), Some((9, 19)));
        assert_eq!(parse_cell_size_response(b"\x1b[6;24;12"), None);
    }

    #[test]
    fn malformed_and_unreasonable_terminal_reports_are_ignored() {
        for bytes in [
            b"\x1b[6;0;12t".as_slice(), b"\x1b[6;24;0t", b"\x1b[6;-1;12t",
            b"\x1b[6;24;12;3t", b"\x1b[4;24;12t", b"\x1b[6;513;12t",
            b"\x1b[6;24;257t", b"\x1b[6;abc;12t", b"\x1b[6;24;999999999999t",
        ] {
            assert_eq!(parse_cell_size_response(bytes), None, "{bytes:?}");
        }
    }

    #[test]
    fn explicit_cell_size_is_validated() {
        assert_eq!(parse_configured_cell_size("12x24"), Some((12, 24)));
        assert_eq!(parse_configured_cell_size(" 9X18 "), Some((9, 18)));
        for value in ["", "0x24", "12x0", "4096x24", "12:24", "12x24x5", "abc"] {
            assert_eq!(parse_configured_cell_size(value), None);
        }
    }
}
