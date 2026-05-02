//! CUETools Database (CTDB) client: TOC lookup, CRC32 verification.
//!
//! CTDB uses standard CRC32 over raw audio PCM bytes (no sample skipping).
//! The API returns per-track CRC32 values and parity data availability
//! for future Reed-Solomon error repair.

use std::path::{Path, PathBuf};

// ── Data structures ─────────────────────────────────────────────────

/// A single entry from the CTDB lookup response.
#[derive(Debug, Clone)]
pub struct CtdbEntry {
    pub id: String,
    pub crc32: u32,
    pub confidence: u32,
    pub npar: u32,
    pub stride: usize,
    pub has_parity: Option<String>,
    pub track_crcs: Vec<u32>,
}

/// Full response from a CTDB lookup.
#[derive(Debug, Clone)]
pub struct CtdbResponse {
    pub entries: Vec<CtdbEntry>,
}

/// Result of verifying a single track against CTDB.
#[derive(Debug, Clone)]
pub struct CtdbTrackResult {
    pub path: PathBuf,
    pub track_number: u32,
    pub status: CtdbTrackStatus,
    pub confidence: Option<u32>,
    pub computed_crc32: u32,
    pub has_parity: bool,
}

/// Status of a single track's CTDB verification.
#[derive(Debug, Clone, PartialEq)]
pub enum CtdbTrackStatus {
    Verified,
    Mismatch,
    NoDiscInDatabase,
    Error(String),
}

/// Overall result of a CTDB album verification.
#[derive(Debug, Clone)]
pub struct CtdbVerifyResult {
    pub tracks: Vec<CtdbTrackResult>,
    pub toc: String,
}

// ── TOC construction ────────────────────────────────────────────────

/// Build a CTDB TOC string from sector offsets WITH 150-frame lead-in.
///
/// CTDB expects colon-separated LBA values (without lead-in):
/// `"0:16032:32072:47282:62810"` where the last value is the leadout.
pub fn build_ctdb_toc(toc_sectors: &[u32]) -> String {
    toc_sectors.iter()
        .map(|&s| s.saturating_sub(150).to_string())
        .collect::<Vec<_>>()
        .join(":")
}

/// Build a CTDB TOC string from sample counts (fallback when no TOC file).
pub fn build_ctdb_toc_from_samples(sample_counts: &[u64], sample_rate: u32) -> String {
    let samples_per_frame = (sample_rate / 75) as u64;
    let mut offsets = Vec::with_capacity(sample_counts.len() + 1);
    let mut frame = 0u64;
    for &count in sample_counts {
        offsets.push(frame.to_string());
        frame += count / samples_per_frame;
    }
    offsets.push(frame.to_string()); // leadout
    offsets.join(":")
}

// ── API client ──────────────────────────────────────────────────────

const CTDB_BASE: &str = "https://db.cue.tools/lookup2.php";

/// Query the CTDB API with a TOC string.
pub async fn query_ctdb(toc: &str) -> Result<Option<CtdbResponse>, String> {
    let url = format!("{}?version=3&ctdb=1&toc={}", CTDB_BASE, toc);
    log::info!("CTDB query: {}", url);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| format!("HTTP client error: {}", e))?;

    let resp = client.get(&url)
        .send()
        .await
        .map_err(|e| format!("CTDB query failed: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("CTDB HTTP {}", resp.status()));
    }

    let body = resp.text()
        .await
        .map_err(|e| format!("CTDB response error: {}", e))?;

    log::info!("CTDB response: {} bytes", body.len());

    let response = parse_ctdb_response(&body)?;
    if response.entries.is_empty() {
        Ok(None)
    } else {
        Ok(Some(response))
    }
}

/// Parse the CTDB XML response.
///
/// Simple attribute extraction — no XML crate needed. The response
/// contains `<entry>` elements with flat attributes.
fn parse_ctdb_response(xml: &str) -> Result<CtdbResponse, String> {
    let mut entries = Vec::new();

    for line in xml.lines() {
        let trimmed = line.trim();
        if !trimmed.contains("<entry ") && !trimmed.starts_with("<entry ") {
            continue;
        }

        let id = extract_attr(trimmed, "id").unwrap_or_default();
        let crc32 = extract_attr(trimmed, "crc32")
            .and_then(|s| u32::from_str_radix(&s, 16).ok())
            .unwrap_or(0);
        let confidence = extract_attr(trimmed, "confidence")
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0);
        let npar = extract_attr(trimmed, "npar")
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(8);
        let stride = extract_attr(trimmed, "stride")
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(11760);
        let has_parity = extract_attr(trimmed, "hasparity");
        let track_crcs: Vec<u32> = extract_attr(trimmed, "trackcrcs")
            .map(|s| s.split_whitespace()
                .filter_map(|h| u32::from_str_radix(h, 16).ok())
                .collect())
            .unwrap_or_default();

        entries.push(CtdbEntry {
            id,
            crc32,
            confidence,
            npar,
            stride,
            has_parity,
            track_crcs,
        });
    }

    Ok(CtdbResponse { entries })
}

/// Extract an XML attribute value: `attr="value"` → `Some("value")`.
fn extract_attr(xml: &str, attr: &str) -> Option<String> {
    let pattern = format!("{}=\"", attr);
    let start = xml.find(&pattern)? + pattern.len();
    let end = xml[start..].find('"')? + start;
    Some(xml[start..end].to_string())
}

// ── CRC32 computation ───────────────────────────────────────────────

/// CTDB stride in 16-bit words (10 CD sectors × 588 samples × 2 channels).
const STRIDE_WORDS: usize = 10 * 588 * 2; // 11760

/// Prefix skip for the first track: stride/2 = 5880 stereo sample pairs = 11760 i16.
const PREFIX_SKIP_I16: usize = STRIDE_WORDS; // 11760

/// Compute the last-track suffix skip in i16 values.
///
/// `total_samples` is the total stereo sample pair count for the entire disc.
/// The formula matches CUETools: `laststride = stride + (total_words % stride)`,
/// then `suffixSamples = laststride / 2`.
pub fn compute_suffix_skip(total_samples: u64) -> usize {
    let total_words = total_samples as usize * 2; // stereo pairs to 16-bit words
    let remainder = total_words % STRIDE_WORDS;
    let laststride = STRIDE_WORDS + remainder;
    laststride // laststride is already in 16-bit words = i16 count
}

/// Compute CRC32 for a track's audio, with CTDB boundary skipping.
///
/// First track: skip `stride/2` (5880) stereo samples from the start.
/// Last track: skip `laststride/2` stereo samples from the end, where
/// `laststride = stride + (total_disc_words % stride)` — album-specific.
/// Middle tracks: no skip.
pub fn compute_track_crc32(
    audio: &[i16],
    is_first: bool,
    is_last: bool,
    suffix_skip_i16: usize, // pre-computed last-track skip (0 for non-last)
) -> u32 {
    let start = if is_first { PREFIX_SKIP_I16.min(audio.len()) } else { 0 };
    let end = if is_last { audio.len().saturating_sub(suffix_skip_i16) } else { audio.len() };
    if start >= end {
        return 0;
    }
    let trimmed = &audio[start..end];
    let bytes: &[u8] = unsafe {
        std::slice::from_raw_parts(
            trimmed.as_ptr() as *const u8,
            trimmed.len() * 2,
        )
    };
    crc32fast::hash(bytes)
}

// ── Verification orchestrator ───────────────────────────────────────

/// Verify an album against the CUETools Database.
///
/// Builds a TOC, queries CTDB, decodes each track, computes CRC32,
/// and compares against the database entries.
pub async fn verify_ctdb(
    paths: &[PathBuf],
    sample_counts: &[u64],
    sample_rate: u32,
) -> CtdbVerifyResult {
    let n = paths.len();

    // Build TOC — try log/CUE file first, fall back to sample counts.
    let dir = paths[0].parent().unwrap_or(Path::new("."));
    let toc = if let Some(toc_sectors) = super::accuraterip::find_toc_offsets(dir) {
        if toc_sectors.len() == n + 1 {
            build_ctdb_toc(&toc_sectors)
        } else {
            build_ctdb_toc_from_samples(sample_counts, sample_rate)
        }
    } else {
        build_ctdb_toc_from_samples(sample_counts, sample_rate)
    };

    log::info!("CTDB TOC: {}", toc);

    // Query CTDB.
    let db_response = match query_ctdb(&toc).await {
        Ok(Some(resp)) => resp,
        Ok(None) => {
            return CtdbVerifyResult {
                tracks: (0..n).map(|i| CtdbTrackResult {
                    path: paths[i].clone(),
                    track_number: (i + 1) as u32,
                    status: CtdbTrackStatus::NoDiscInDatabase,
                    confidence: None,
                    computed_crc32: 0,
                    has_parity: false,
                }).collect(),
                toc,
            };
        }
        Err(e) => {
            return CtdbVerifyResult {
                tracks: (0..n).map(|i| CtdbTrackResult {
                    path: paths[i].clone(),
                    track_number: (i + 1) as u32,
                    status: CtdbTrackStatus::Error(e.clone()),
                    confidence: None,
                    computed_crc32: 0,
                    has_parity: false,
                }).collect(),
                toc,
            };
        }
    };

    // Find the best matching entry (highest confidence).
    let best_entry = db_response.entries.iter()
        .max_by_key(|e| e.confidence);

    let entry = match best_entry {
        Some(e) => e,
        None => {
            return CtdbVerifyResult {
                tracks: (0..n).map(|i| CtdbTrackResult {
                    path: paths[i].clone(),
                    track_number: (i + 1) as u32,
                    status: CtdbTrackStatus::NoDiscInDatabase,
                    confidence: None,
                    computed_crc32: 0,
                    has_parity: false,
                }).collect(),
                toc,
            };
        }
    };

    // Compute the last-track suffix skip from total disc samples.
    let total_disc_samples: u64 = sample_counts.iter().sum();
    let suffix_skip = compute_suffix_skip(total_disc_samples);

    // Decode each track and compute CRC32.
    let mut decode_handles = Vec::with_capacity(n);
    for path in paths.iter() {
        let path_clone = path.clone();
        let handle = tokio::task::spawn_blocking(move || {
            super::accuraterip::decode_track_to_raw_i16(&path_clone)
        });
        decode_handles.push(handle);
    }

    let mut results = Vec::with_capacity(n);
    for (i, handle) in decode_handles.into_iter().enumerate() {
        let decoded = match handle.await {
            Ok(Ok(data)) => data,
            Ok(Err(e)) => {
                results.push(CtdbTrackResult {
                    path: paths[i].clone(),
                    track_number: (i + 1) as u32,
                    status: CtdbTrackStatus::Error(e),
                    confidence: None,
                    computed_crc32: 0,
                    has_parity: entry.has_parity.is_some(),
                });
                continue;
            }
            Err(e) => {
                results.push(CtdbTrackResult {
                    path: paths[i].clone(),
                    track_number: (i + 1) as u32,
                    status: CtdbTrackStatus::Error(format!("decode failed: {}", e)),
                    confidence: None,
                    computed_crc32: 0,
                    has_parity: entry.has_parity.is_some(),
                });
                continue;
            }
        };

        let is_first = i == 0;
        let is_last = i == n - 1;
        let computed = compute_track_crc32(&decoded, is_first, is_last, suffix_skip);
        let db_crc = entry.track_crcs.get(i).copied();

        let status = match db_crc {
            Some(expected) if expected == computed => CtdbTrackStatus::Verified,
            Some(_) => CtdbTrackStatus::Mismatch,
            None => CtdbTrackStatus::Error("track not in CTDB entry".to_string()),
        };

        results.push(CtdbTrackResult {
            path: paths[i].clone(),
            track_number: (i + 1) as u32,
            status,
            confidence: Some(entry.confidence),
            computed_crc32: computed,
            has_parity: entry.has_parity.is_some(),
        });
    }

    CtdbVerifyResult { tracks: results, toc }
}

/// Format a summary string for CTDB verification results.
pub fn format_ctdb_summary(result: &CtdbVerifyResult) -> String {
    let total = result.tracks.len();
    let verified = result.tracks.iter()
        .filter(|t| t.status == CtdbTrackStatus::Verified)
        .count();

    if result.tracks.iter().any(|t| t.status == CtdbTrackStatus::NoDiscInDatabase) {
        return "Disc not in CUETools database".to_string();
    }

    if verified == 0 {
        return format!("0/{} tracks verified", total);
    }

    let max_conf = result.tracks.iter()
        .filter_map(|t| t.confidence)
        .max()
        .unwrap_or(0);

    let has_parity = result.tracks.iter().any(|t| t.has_parity);
    let parity_str = if has_parity { ", parity available" } else { "" };

    format!(
        "{}/{} verified, confidence {}{}",
        verified, total, max_conf, parity_str,
    )
}

/// Verify a single-image CUE album against CTDB.
///
/// Decodes the full image, splits by CUE boundaries, computes per-track
/// CRC32, and compares against CTDB entries.
pub async fn verify_ctdb_single_image(
    info: &super::cue_parser::SingleImageInfo,
) -> CtdbVerifyResult {
    let n = info.track_boundaries.len();

    // Build TOC from CUE INDEX timestamps.
    let toc = {
        let toc_sectors = super::accuraterip::find_toc_offsets(
            info.cue_path.parent().unwrap_or(Path::new(".")),
        );
        if let Some(ref sectors) = toc_sectors {
            if sectors.len() == n + 1 {
                build_ctdb_toc(sectors)
            } else {
                let sample_counts: Vec<u64> = info.track_boundaries.iter()
                    .map(|&(_, count)| count).collect();
                build_ctdb_toc_from_samples(&sample_counts, info.sample_rate)
            }
        } else {
            let sample_counts: Vec<u64> = info.track_boundaries.iter()
                .map(|&(_, count)| count).collect();
            build_ctdb_toc_from_samples(&sample_counts, info.sample_rate)
        }
    };

    // Query CTDB.
    let db_response = match query_ctdb(&toc).await {
        Ok(Some(resp)) => resp,
        Ok(None) => {
            return CtdbVerifyResult {
                tracks: (0..n).map(|i| CtdbTrackResult {
                    path: info.audio_path.clone(),
                    track_number: (i + 1) as u32,
                    status: CtdbTrackStatus::NoDiscInDatabase,
                    confidence: None,
                    computed_crc32: 0,
                    has_parity: false,
                }).collect(),
                toc,
            };
        }
        Err(e) => {
            return CtdbVerifyResult {
                tracks: (0..n).map(|i| CtdbTrackResult {
                    path: info.audio_path.clone(),
                    track_number: (i + 1) as u32,
                    status: CtdbTrackStatus::Error(e.clone()),
                    confidence: None,
                    computed_crc32: 0,
                    has_parity: false,
                }).collect(),
                toc,
            };
        }
    };

    let entry = match db_response.entries.iter().max_by_key(|e| e.confidence) {
        Some(e) => e,
        None => {
            return CtdbVerifyResult {
                tracks: (0..n).map(|i| CtdbTrackResult {
                    path: info.audio_path.clone(),
                    track_number: (i + 1) as u32,
                    status: CtdbTrackStatus::NoDiscInDatabase,
                    confidence: None,
                    computed_crc32: 0,
                    has_parity: false,
                }).collect(),
                toc,
            };
        }
    };

    // Decode the full image. Try ffmpeg, fall back to wvunpack.
    let audio_path = info.audio_path.clone();
    let raw_result = tokio::task::spawn_blocking(move || {
        super::accuraterip::decode_track_to_raw_i16(&audio_path)
            .or_else(|_| super::accuraterip::decode_to_raw_i16_wvunpack(&audio_path))
    }).await;

    let raw_i16 = match raw_result {
        Ok(Ok(data)) => data,
        Ok(Err(e)) => {
            return CtdbVerifyResult {
                tracks: (0..n).map(|i| CtdbTrackResult {
                    path: info.audio_path.clone(),
                    track_number: (i + 1) as u32,
                    status: CtdbTrackStatus::Error(e.clone()),
                    confidence: None,
                    computed_crc32: 0,
                    has_parity: false,
                }).collect(),
                toc,
            };
        }
        Err(e) => {
            return CtdbVerifyResult {
                tracks: (0..n).map(|i| CtdbTrackResult {
                    path: info.audio_path.clone(),
                    track_number: (i + 1) as u32,
                    status: CtdbTrackStatus::Error(format!("decode failed: {}", e)),
                    confidence: None,
                    computed_crc32: 0,
                    has_parity: false,
                }).collect(),
                toc,
            };
        }
    };

    // Compute per-track CRC32 from segments.
    let mut results = Vec::with_capacity(n);
    for (i, &(start_sample, sample_count)) in info.track_boundaries.iter().enumerate() {
        let start = start_sample as usize * 2; // i16 index (2 i16s per stereo sample)
        let count = sample_count as usize * 2;
        let end = (start + count).min(raw_i16.len());

        if start >= raw_i16.len() {
            results.push(CtdbTrackResult {
                path: info.audio_path.clone(),
                track_number: (i + 1) as u32,
                status: CtdbTrackStatus::Error("track beyond audio data".to_string()),
                confidence: None,
                computed_crc32: 0,
                has_parity: entry.has_parity.is_some(),
            });
            continue;
        }

        let track_audio = &raw_i16[start..end];
        let is_first = i == 0;
        let is_last = i == n - 1;
        let suffix_skip = compute_suffix_skip(info.total_samples);
        let computed = compute_track_crc32(track_audio, is_first, is_last, suffix_skip);
        let db_crc = entry.track_crcs.get(i).copied();

        let status = match db_crc {
            Some(expected) if expected == computed => CtdbTrackStatus::Verified,
            Some(_) => CtdbTrackStatus::Mismatch,
            None => CtdbTrackStatus::Error("track not in CTDB entry".to_string()),
        };

        results.push(CtdbTrackResult {
            path: info.audio_path.clone(),
            track_number: (i + 1) as u32,
            status,
            confidence: Some(entry.confidence),
            computed_crc32: computed,
            has_parity: entry.has_parity.is_some(),
        });
    }

    CtdbVerifyResult { tracks: results, toc }
}
