//! Diagnostic: decode each track, apply our CTDB CRC32 logic, print
//! computed vs expected for the canonical CTDB entry.
//!
//! Usage:
//!   cargo run --example ctdb_diag -- <track1.flac> <track2.flac> ...
//!
//! Then paste the canonical entry's `trackcrcs` value to compare.

use std::path::PathBuf;
use std::process::Command;

const STRIDE_WORDS: usize = 10 * 588 * 2; // 11760 i16
const PREFIX_SKIP_I16: usize = STRIDE_WORDS;

fn compute_suffix_skip(total_samples: u64) -> usize {
    let total_words = total_samples as usize * 2;
    let remainder = total_words % STRIDE_WORDS;
    STRIDE_WORDS + remainder
}

fn compute_track_crc32(
    audio: &[i16],
    is_first: bool,
    is_last: bool,
    suffix_skip_i16: usize,
) -> u32 {
    let start = if is_first {
        PREFIX_SKIP_I16.min(audio.len())
    } else {
        0
    };
    let end = if is_last {
        audio.len().saturating_sub(suffix_skip_i16)
    } else {
        audio.len()
    };
    if start >= end {
        return 0;
    }
    let trimmed = &audio[start..end];
    let bytes: &[u8] =
        unsafe { std::slice::from_raw_parts(trimmed.as_ptr() as *const u8, trimmed.len() * 2) };
    crc32fast::hash(bytes)
}

fn decode_flac_to_i16(path: &PathBuf) -> Vec<i16> {
    let output = Command::new("ffmpeg")
        .args(["-y", "-hide_banner", "-loglevel", "error", "-i"])
        .arg(path)
        .args(["-f", "s16le", "-ar", "44100", "-ac", "2", "-"])
        .output()
        .expect("ffmpeg failed");
    if !output.status.success() {
        panic!(
            "ffmpeg decode failed for {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    output
        .stdout
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("usage: ctdb_diag <track1> <track2> ...");
        std::process::exit(1);
    }
    let paths: Vec<PathBuf> = args.into_iter().map(PathBuf::from).collect();
    let n = paths.len();

    println!("Decoding {} tracks...", n);
    let mut all_audio: Vec<Vec<i16>> = Vec::with_capacity(n);
    for path in &paths {
        let audio = decode_flac_to_i16(path);
        println!(
            "  {} -> {} i16 ({} stereo pairs)",
            path.display(),
            audio.len(),
            audio.len() / 2
        );
        all_audio.push(audio);
    }

    let total_disc_samples: u64 = all_audio.iter().map(|a| (a.len() / 2) as u64).sum();
    let suffix_skip = compute_suffix_skip(total_disc_samples);
    println!();
    println!("total_disc_samples = {} stereo pairs", total_disc_samples);
    println!(
        "compute_suffix_skip = {} i16 ({} stereo pairs)",
        suffix_skip,
        suffix_skip / 2
    );
    println!(
        "PREFIX_SKIP_I16 = {} ({} stereo pairs)",
        PREFIX_SKIP_I16,
        PREFIX_SKIP_I16 / 2
    );
    println!();

    // Canonical CTDB entry trackcrcs: "40c5dc10 65dfcc8a 1ef7b539 d21a8789"
    let expected_canonical = ["40c5dc10", "65dfcc8a", "1ef7b539", "d21a8789"];

    println!("Computed CRC32 per track (our code's logic):");
    for (i, audio) in all_audio.iter().enumerate() {
        let is_first = i == 0;
        let is_last = i == n - 1;
        let crc = compute_track_crc32(audio, is_first, is_last, suffix_skip);
        let expected = expected_canonical.get(i).unwrap_or(&"?");
        let mark = if format!("{:08x}", crc) == *expected {
            "✓"
        } else {
            "✗"
        };
        println!(
            "  Track {}: {:08x}  expected {}  {}",
            i + 1,
            crc,
            expected,
            mark
        );
    }
}
