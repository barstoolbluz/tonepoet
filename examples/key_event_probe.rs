//! Minimal terminal key-delivery probe for baseline crossterm input.
//!
//! Run this *inside the same byobu/tmux session used for tonepoet*:
//!
//!     cargo run --example key_event_probe
//!
//! Press plain Backspace, then physical Ctrl+Backspace. The program reports
//! the exact crossterm `KeyEvent` for each press and restores terminal raw mode
//! on every normal/error exit via RAII.

use std::io;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};

struct RawModeGuard;

impl RawModeGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
    }
}

fn read_press(label: &str) -> io::Result<KeyEvent> {
    eprintln!("Press {label} once.");
    loop {
        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            // In raw mode Ctrl+C is delivered as a key event rather than a
            // signal. Keep an explicit escape hatch so the probe cannot trap
            // the terminal session if the user changes their mind.
            if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "probe cancelled with Ctrl+C",
                ));
            }
            return Ok(key);
        }
    }
}

fn report(label: &str, key: &KeyEvent) {
    println!(
        "{label}: code={:?}, modifiers={:?}, kind={:?}, state={:?}",
        key.code, key.modifiers, key.kind, key.state
    );
}

fn is_plain_backspace(key: &KeyEvent) -> bool {
    key.code == KeyCode::Backspace
        && !key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::ALT)
}

fn is_supported_strong_delete_delivery(key: &KeyEvent) -> bool {
    let control_backspace = key.code == KeyCode::Backspace
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::ALT);
    let baseline_ctrl_h = matches!(key.code, KeyCode::Char(c) if c.eq_ignore_ascii_case(&'h'))
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::ALT);
    control_backspace || baseline_ctrl_h
}

fn main() -> io::Result<()> {
    eprintln!("tonepoet baseline key-delivery probe (run inside your normal byobu/tmux pane)");
    eprintln!("Ctrl+C cancels. No keyboard-enhancement protocol is enabled.");
    let _raw_mode = RawModeGuard::enter()?;

    let plain = read_press("plain Backspace")?;
    let strong = read_press("physical Ctrl+Backspace")?;

    // Restore cooked mode before printing the final copy/paste-friendly report.
    drop(_raw_mode);
    report("plain Backspace", &plain);
    report("Ctrl+Backspace", &strong);

    if is_plain_backspace(&plain) && is_supported_strong_delete_delivery(&strong) {
        println!(
            "RESULT: target key delivery is compatible with tonepoet's shared text-input mapping"
        );
        Ok(())
    } else {
        println!(
            "RESULT: target key delivery is NOT confirmed compatible; retain these events and \
             adjust only the shared text-input mapping if required"
        );
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unexpected Backspace/Ctrl+Backspace delivery",
        ))
    }
}
