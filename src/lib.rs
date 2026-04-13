pub mod convert;
pub mod config;
pub mod tui;

/// Detect the best available 7-Zip binary. Prefers the official `7zz`
/// (native Linux 7-Zip with SIMD optimizations, ~2-3x faster than p7zip)
/// over the legacy `7z` (p7zip). Returns the binary name or None.
pub fn detect_7z_binary() -> Option<&'static str> {
    use std::process::Command;
    // Prefer 7zz (official 7-Zip for Linux, v21+)
    if Command::new("7zz")
        .arg("--help")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
    {
        return Some("7zz");
    }
    // Fall back to 7z (p7zip)
    if Command::new("7z")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
    {
        return Some("7z");
    }
    None
}
