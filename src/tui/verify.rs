//! Audio file integrity verification via native tools or ffmpeg decode.

use std::path::{Path, PathBuf};
use tokio::process::Command;

/// Result of verifying a single audio file.
#[derive(Debug, Clone)]
pub struct VerifyResult {
    pub path: PathBuf,
    pub passed: bool,
    pub detail: String,
}

/// Verify a single audio file's integrity.
///
/// Strategy by format:
/// - FLAC: `flac --test --silent` (checks frame CRCs + MD5 of decoded audio)
/// - WavPack: `wvunpack -vq` (verifies block checksums)
/// - Everything else: `ffmpeg -v error -i <path> -f null -` (full decode; any
///   stderr output indicates corruption)
pub async fn verify_file(path: PathBuf) -> VerifyResult {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    let (passed, detail) = match ext.as_str() {
        "flac" => verify_flac(&path).await,
        "wv" => verify_wavpack(&path).await,
        _ => verify_ffmpeg(&path).await,
    };

    VerifyResult {
        path,
        passed,
        detail,
    }
}

async fn verify_flac(path: &Path) -> (bool, String) {
    match Command::new("flac")
        .args(["--test", "--silent"])
        .arg(path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .await
    {
        Ok(output) if output.status.success() => (true, "FLAC stream ok (MD5 verified)".into()),
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let msg = first_meaningful_line(&stderr).unwrap_or("verification failed");
            (false, msg.to_string())
        }
        Err(e) => (false, format!("flac not found: {}", e)),
    }
}

async fn verify_wavpack(path: &Path) -> (bool, String) {
    match Command::new("wvunpack")
        .args(["-vq"])
        .arg(path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .await
    {
        Ok(output) if output.status.success() => (true, "WavPack stream ok".into()),
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let msg = first_meaningful_line(&stderr).unwrap_or("verification failed");
            (false, msg.to_string())
        }
        Err(e) => (false, format!("wvunpack not found: {}", e)),
    }
}

async fn verify_ffmpeg(path: &Path) -> (bool, String) {
    match Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-f", "null", "-"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .await
    {
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let trimmed = stderr.trim();
            if trimmed.is_empty() && output.status.success() {
                (true, "decode ok".into())
            } else if trimmed.is_empty() {
                (false, format!("exit code {}", output.status))
            } else {
                let msg = first_meaningful_line(trimmed).unwrap_or("decode error");
                (false, msg.to_string())
            }
        }
        Err(e) => (false, format!("ffmpeg not found: {}", e)),
    }
}

/// Extract the first non-empty line from error output, truncated to a
/// reasonable length for display.
fn first_meaningful_line(text: &str) -> Option<&str> {
    text.lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .map(|l| {
            if l.len() <= 120 {
                l
            } else {
                // Truncate at a char boundary.
                let mut end = 120;
                while end > 0 && !l.is_char_boundary(end) {
                    end -= 1;
                }
                &l[..end]
            }
        })
}
