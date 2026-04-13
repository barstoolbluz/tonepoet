//! Archive password keychain: MRU-ordered plaintext password list.
//!
//! Passwords are stored in `~/.config/tonepoet/passwords.toml` with
//! restrictive file permissions (0600 on Unix). The list is MRU-ordered:
//! successfully-used passwords float to the top on each use.
//!
//! The keychain is NOT a security vault — it stores plaintext passwords
//! for convenience when working with encrypted archives (7z, rar, zip).

use std::path::PathBuf;

/// Return the path to the password keychain file.
pub fn keychain_path() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        PathBuf::from(xdg)
            .join("tonepoet")
            .join("passwords.toml")
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home)
            .join(".config")
            .join("tonepoet")
            .join("passwords.toml")
    } else {
        PathBuf::from("passwords.toml")
    }
}

/// On-disk format. The `passwords` field is an ordered list (MRU first).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct KeychainFile {
    #[serde(default)]
    passwords: Vec<String>,
}

/// Load the keychain (MRU-ordered). Returns an empty vec if the file
/// doesn't exist or can't be parsed.
pub fn load_keychain() -> Vec<String> {
    let path = keychain_path();
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let file: KeychainFile = toml::from_str(&content).unwrap_or_default();
    file.passwords
}

/// Save the keychain to disk. Creates parent directories and sets
/// file permissions to 0600 on Unix.
pub fn save_keychain(passwords: &[String]) -> Result<(), String> {
    let path = keychain_path();
    let file = KeychainFile {
        passwords: passwords.to_vec(),
    };
    let toml_str =
        toml::to_string_pretty(&file).map_err(|e| format!("serialize error: {}", e))?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir error: {}", e))?;
    }
    std::fs::write(&path, &toml_str).map_err(|e| format!("write error: {}", e))?;

    // Restrict permissions on Unix (owner read/write only).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        let _ = std::fs::set_permissions(&path, perms);
    }

    Ok(())
}

/// Add a password to the keychain. If it already exists, moves it to
/// the front (MRU). Otherwise prepends it. Saves to disk.
pub fn add_password(password: &str) -> Result<(), String> {
    let mut passwords = load_keychain();
    // Remove existing occurrence (if any) to re-insert at front.
    passwords.retain(|p| p != password);
    passwords.insert(0, password.to_string());
    save_keychain(&passwords)
}

/// Remove a password from the keychain by index. Saves to disk.
pub fn remove_password(index: usize) -> Result<(), String> {
    let mut passwords = load_keychain();
    if index >= passwords.len() {
        return Err(format!("index {} out of range ({})", index, passwords.len()));
    }
    passwords.remove(index);
    save_keychain(&passwords)
}

/// Promote a password to MRU position (index 0). Called when a
/// password successfully unlocks an archive.
pub fn promote_password(password: &str) -> Result<(), String> {
    // add_password already handles remove-then-prepend.
    add_password(password)
}

/// Test an archive with a specific password using the best available
/// 7-Zip binary (`7zz` preferred over `7z`).
/// Returns Ok(true) if the password works, Ok(false) if wrong password,
/// Err on tool failure.
pub async fn test_password(archive: &std::path::Path, password: &str) -> Result<bool, String> {
    use tokio::process::Command;

    let bin = crate::detect_7z_binary()
        .ok_or_else(|| "neither 7zz nor 7z found in PATH".to_string())?;

    let mut cmd = Command::new(bin);
    cmd.arg("t")
        .arg(archive)
        .arg(format!("-p{}", password))
        .arg("-y")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());

    let output = cmd
        .output()
        .await
        .map_err(|e| format!("failed to run {}: {}", bin, e))?;

    // 7z/7zz exit codes: 0 = ok, 2 = fatal error (includes wrong password)
    Ok(output.status.success())
}

/// Try all keychain passwords against an archive. Returns the first
/// working password, or None if none work.
pub async fn try_keychain(archive: &std::path::Path) -> Option<String> {
    let passwords = load_keychain();
    for pw in &passwords {
        match test_password(archive, pw).await {
            Ok(true) => {
                // Promote to MRU on success (best-effort, don't fail the unlock).
                let _ = promote_password(pw);
                return Some(pw.clone());
            }
            _ => continue,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_serialize() {
        let file = KeychainFile {
            passwords: vec!["alpha".into(), "bravo".into(), "charlie".into()],
        };
        let toml_str = toml::to_string_pretty(&file).unwrap();
        let parsed: KeychainFile = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.passwords, vec!["alpha", "bravo", "charlie"]);
    }

    #[test]
    fn empty_file_parses() {
        let parsed: KeychainFile = toml::from_str("").unwrap_or_default();
        assert!(parsed.passwords.is_empty());
    }

    #[test]
    fn mru_dedup() {
        // Simulate add_password behavior without disk I/O.
        let mut passwords = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let new = "b";
        passwords.retain(|p| p != new);
        passwords.insert(0, new.to_string());
        assert_eq!(passwords, vec!["b", "a", "c"]);
    }
}
