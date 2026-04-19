//! Bit-level audio comparison: decode two files to raw PCM via ffmpeg
//! and compare the byte streams chunk by chunk.

use std::path::{Path, PathBuf};
use tokio::io::AsyncReadExt;
use tokio::process::Command;

/// Result of comparing two audio files at the PCM level.
#[derive(Debug, Clone)]
pub struct CompareResult {
    pub ref_path: PathBuf,
    pub target_path: PathBuf,
    pub identical: bool,
    pub detail: String,
}

/// Compare two audio files by decoding both to raw s32le PCM and
/// comparing the byte streams. Returns immediately with "incompatible"
/// if sample rate, channels, or bit depth differ.
pub async fn compare_files(ref_path: PathBuf, target_path: PathBuf) -> CompareResult {
    // Quick compatibility check via probe.
    let ref_info = match tokio::task::spawn_blocking({
        let p = ref_path.clone();
        move || crate::tui::probe::probe_audio(&p)
    }).await {
        Ok(Ok(info)) => info,
        Ok(Err(e)) => return incompatible(&ref_path, &target_path, &format!("probe ref: {}", e)),
        Err(e) => return incompatible(&ref_path, &target_path, &format!("probe ref: {}", e)),
    };

    let target_info = match tokio::task::spawn_blocking({
        let p = target_path.clone();
        move || crate::tui::probe::probe_audio(&p)
    }).await {
        Ok(Ok(info)) => info,
        Ok(Err(e)) => return incompatible(&ref_path, &target_path, &format!("probe target: {}", e)),
        Err(e) => return incompatible(&ref_path, &target_path, &format!("probe target: {}", e)),
    };

    if ref_info.sample_rate != target_info.sample_rate {
        return incompatible(&ref_path, &target_path, &format!(
            "sample rate: {} vs {}",
            ref_info.sample_rate, target_info.sample_rate,
        ));
    }
    if ref_info.channels != target_info.channels {
        return incompatible(&ref_path, &target_path, &format!(
            "channels: {} vs {}",
            ref_info.channels, target_info.channels,
        ));
    }
    if ref_info.bit_depth != target_info.bit_depth {
        return incompatible(&ref_path, &target_path, &format!(
            "bit depth: {:?} vs {:?}",
            ref_info.bit_depth, target_info.bit_depth,
        ));
    }

    // Decode both to raw s32le PCM via ffmpeg pipes.
    let mut ref_child = match spawn_decoder(&ref_path) {
        Ok(c) => c,
        Err(e) => return incompatible(&ref_path, &target_path, &format!("decode ref: {}", e)),
    };
    let mut target_child = match spawn_decoder(&target_path) {
        Ok(c) => c,
        Err(e) => {
            let _ = ref_child.kill().await;
            return incompatible(&ref_path, &target_path, &format!("decode target: {}", e));
        }
    };

    let mut ref_stdout = ref_child.stdout.take().unwrap();
    let mut target_stdout = target_child.stdout.take().unwrap();

    const CHUNK: usize = 65536;
    let mut ref_buf = vec![0u8; CHUNK];
    let mut target_buf = vec![0u8; CHUNK];
    let mut offset: u64 = 0;

    let result = loop {
        // Fill both buffers completely (read_exact), detecting EOF.
        let (ref_res, target_res) = tokio::join!(
            fill_buf(&mut ref_stdout, &mut ref_buf),
            fill_buf(&mut target_stdout, &mut target_buf),
        );

        let ref_n = match ref_res {
            Ok(n) => n,
            Err(e) => break incompatible(&ref_path, &target_path, &format!("read ref: {}", e)),
        };
        let target_n = match target_res {
            Ok(n) => n,
            Err(e) => break incompatible(&ref_path, &target_path, &format!("read target: {}", e)),
        };

        // Compare the common prefix.
        let common = ref_n.min(target_n);
        if common > 0 && ref_buf[..common] != target_buf[..common] {
            let diff_pos = ref_buf[..common].iter()
                .zip(target_buf[..common].iter())
                .position(|(a, b)| a != b)
                .unwrap_or(0);
            let byte_offset = offset + diff_pos as u64;
            let channels = ref_info.channels as u64;
            let bytes_per_sample_frame = 4 * channels;
            let sample_offset = byte_offset / bytes_per_sample_frame;
            let time_secs = sample_offset as f64 / ref_info.sample_rate as f64;
            let mins = time_secs as u64 / 60;
            let secs = time_secs as u64 % 60;
            break CompareResult {
                ref_path: ref_path.clone(),
                target_path: target_path.clone(),
                identical: false,
                detail: format!(
                    "differ at sample {} ({:02}:{:02})",
                    sample_offset, mins, secs,
                ),
            };
        }

        if ref_n != target_n {
            break CompareResult {
                ref_path: ref_path.clone(),
                target_path: target_path.clone(),
                identical: false,
                detail: format!(
                    "different length ({} is longer)",
                    if ref_n > target_n { "reference" } else { "target" },
                ),
            };
        }

        if ref_n == 0 {
            // Both streams ended with matching data — identical.
            break CompareResult {
                ref_path: ref_path.clone(),
                target_path: target_path.clone(),
                identical: true,
                detail: "bit-identical".into(),
            };
        }

        offset += ref_n as u64;
    };

    // Clean up child processes.
    let _ = ref_child.kill().await;
    let _ = target_child.kill().await;

    result
}

/// Read exactly `buf.len()` bytes, or fewer only at EOF.
/// Returns the number of bytes actually read.
async fn fill_buf(
    reader: &mut (impl AsyncReadExt + Unpin),
    buf: &mut [u8],
) -> Result<usize, std::io::Error> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]).await? {
            0 => break, // EOF
            n => filled += n,
        }
    }
    Ok(filled)
}

fn spawn_decoder(path: &Path) -> Result<tokio::process::Child, String> {
    Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-f", "s32le", "-acodec", "pcm_s32le", "pipe:1"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("ffmpeg: {}", e))
}

fn incompatible(ref_path: &Path, target_path: &Path, detail: &str) -> CompareResult {
    CompareResult {
        ref_path: ref_path.to_path_buf(),
        target_path: target_path.to_path_buf(),
        identical: false,
        detail: format!("incompatible: {}", detail),
    }
}
