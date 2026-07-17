pub mod config;
pub mod convert;
pub mod ctdb_rs;
pub mod db;
pub mod disc;
pub mod dsf_tags;
pub mod secret_store;
pub mod tui;

/// Check whether a file path has an archive extension that may be
/// password-encrypted (7z, rar, zip). Tar-based archives and ISO images
/// don't support encryption and are excluded.
pub fn is_encrypted_archive_ext(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| matches!(e.to_lowercase().as_str(), "7z" | "rar" | "zip"))
        .unwrap_or(false)
}

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
        .stdin(std::process::Stdio::null())
        .status()
        .is_ok()
    {
        return Some("7zz");
    }
    // Fall back to 7z (p7zip)
    if Command::new("7z")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null())
        .status()
        .is_ok()
    {
        return Some("7z");
    }
    None
}
