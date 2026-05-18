//! CUETools Database (CTDB) client: TOC lookup, CRC32 verification.
//!
//! CTDB uses standard CRC32 over raw audio PCM bytes (no sample skipping).
//! The API returns per-track CRC32 values and parity data availability
//! for future Reed-Solomon error repair.

use super::message::AppMessage;
use base64::Engine as _;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tokio::sync::mpsc;

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
    pub parity: Option<String>,
    pub syndrome: Option<String>,
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
    /// Expected CRC32 from the CTDB entry (for repair verification).
    pub expected_crc32: Option<u32>,
    pub has_parity: bool,
}

/// Status of a single track's CTDB verification.
#[derive(Debug, Clone, PartialEq)]
pub enum CtdbTrackStatus {
    Verified,
    /// Reed-Solomon verified against CTDB, but per-track CRC32 differs from the entry.
    VerifiedRs,
    Mismatch,
    NoDiscInDatabase,
    Error(String),
}

/// Overall result of a CTDB album verification.
#[derive(Debug, Clone)]
pub struct CtdbVerifyResult {
    pub tracks: Vec<CtdbTrackResult>,
    pub toc: String,
    /// Parity symbol count from the best CTDB entry (for repair).
    pub npar: Option<u32>,
    /// Stride from the best CTDB entry (for repair).
    pub stride: Option<usize>,
    /// Parity download URL from the best CTDB entry (for repair).
    pub parity_url: Option<String>,
    /// When the verify path computed a fresh parity matrix (i.e. didn't
    /// receive a cached one), this carries `(cache_key, parity)` for the
    /// caller to persist via `Database::store_ctdb_parity`. The event-loop
    /// `CtdbComplete` handler must `take()` this field immediately after
    /// the verify task completes — leaving it populated would propagate
    /// a ~376 KB matrix into long-lived overlay state.
    pub parity_cache_write: Option<(String, Vec<Vec<u16>>)>,
}

// ── TOC construction ────────────────────────────────────────────────

/// Build a CTDB TOC string from sector offsets WITH 150-frame lead-in.
///
/// CTDB expects colon-separated LBA values (without lead-in):
/// `"0:16032:32072:47282:62810"` where the last value is the leadout.
pub fn build_ctdb_toc(toc_sectors: &[u32]) -> String {
    toc_sectors
        .iter()
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

/// Default RS offset trial window, in CD stereo sample pairs. CUETools'
/// `FindOffset` searches `[1 - stride/2 .. stride/2)`, which is `±5879`
/// for STRIDE=11_760. `verify_disc_via_rs` re-clips the requested window
/// against the source range internally, so callers can pass this constant
/// without further adjustment.
const CTDB_RS_OFFSET_WINDOW_SAMPLES: i32 = (crate::ctdb_rs::STRIDE as i32 / 2) - 1;

/// Query the CTDB API with a TOC string.
pub async fn query_ctdb(toc: &str) -> Result<Option<CtdbResponse>, String> {
    let url = format!("{}?version=3&ctdb=1&toc={}", CTDB_BASE, toc);
    log::info!("CTDB query: {}", url);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| format!("HTTP client error: {}", e))?;

    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("CTDB query failed: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("CTDB HTTP {}", resp.status()));
    }

    let body = resp
        .text()
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
        // CTDB reports stride in stereo sample pairs. The RS codec operates on
        // 16-bit words, matching CUETools.NET DBEntry's `ctdbRespEntry.stride * 2`.
        let stride = extract_attr(trimmed, "stride")
            .and_then(|s| s.parse::<usize>().ok())
            .map(|s| s * 2)
            .unwrap_or(crate::ctdb_rs::STRIDE);
        let has_parity = extract_attr(trimmed, "hasparity");
        let parity = extract_attr(trimmed, "parity");
        let syndrome = extract_attr(trimmed, "syndrome");
        let track_crcs: Vec<u32> = extract_attr(trimmed, "trackcrcs")
            .map(|s| {
                s.split_whitespace()
                    .filter_map(|h| u32::from_str_radix(h, 16).ok())
                    .collect()
            })
            .unwrap_or_default();

        entries.push(CtdbEntry {
            id,
            crc32,
            confidence,
            npar,
            stride,
            has_parity,
            parity,
            syndrome,
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

// ── Reed-Solomon verification (CUETools-source-faithful translation) ──
//
// This block is a literal translation of CUETools.NET's CTDB FindOffset
// fast-path, sourced from the gchudov/cuetools.net repository. Each
// non-trivial helper carries a `// matches <file>:<lines>` annotation
// pointing at the C# original.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RsVerifySource {
    /// CTDB XML `syndrome="..."` — bytes are already a CUETools syndrome row.
    Syndrome,
    /// Legacy CTDB XML `parity="..."` — needs Parity2Syndrome conversion.
    InlineParity,
    /// Reserved for full-parity blob fallback (not currently used; CUETools'
    /// FindOffset itself uses only the one-row syndrome path).
    FullParity,
}

#[derive(Debug, Clone)]
pub struct RsVerifiedMatch {
    pub entry: CtdbEntry,
    /// CUETools-style actualOffset, in stereo sample pairs.
    pub offset: i32,
    pub confidence: u32,
    pub npar: usize,
    pub source: RsVerifySource,
    pub column0_errors: usize,
}

const CUETOOLS_MAX_NPAR: usize = 16;

/// Compute a content-addressed cache key for the parity matrix of a disc's
/// audio inputs. SHA-256 hex digest of `(path bytes, mtime u64 LE, size u64 LE)`
/// for each path in caller order, separated by a tag byte. Returns `None` if
/// any path can't be stat'd — caller should treat as a cache miss and skip
/// caching rather than risk a stale entry.
pub fn compute_ctdb_parity_cache_key(paths: &[PathBuf]) -> Option<String> {
    if paths.is_empty() {
        return None;
    }

    let mut hasher = Sha256::new();
    hasher.update(b"tonepoet-ctdb-cuetools-middle-span-v2\0");

    for path in paths {
        let meta = std::fs::metadata(path).ok()?;
        let mtime = meta
            .modified()
            .map(crate::db::systemtime_to_unix)
            .unwrap_or(0) as u64;
        let size = meta.len();

        hasher.update(path.to_string_lossy().as_bytes());
        hasher.update(mtime.to_le_bytes());
        hasher.update(size.to_le_bytes());
        hasher.update([0u8]);
    }

    Some(format!("{:x}", hasher.finalize()))
}

/// Compute the audio's full CTDB parity matrix at maxNpar=16. Synchronous
/// CPU-bound work; orchestrators should run this in `spawn_blocking`.
pub fn compute_audio_parity16(audio: &[i16]) -> Option<Vec<Vec<u16>>> {
    let gf = crate::ctdb_rs::Galois16::new();
    let middle_image = cuetools_middle_span_image_for_parity(audio)?;
    crate::ctdb_rs::syndrome::compute_parity_matrix_from_audio(
        &gf,
        &middle_image,
        CUETOOLS_MAX_NPAR,
    )
    .ok()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CuetoolsDiscSpan {
    /// The real decoded PCM payload inside tonepoet's padded image.
    payload_start: usize,
    payload_len: usize,
    /// CUETools CDRepair.stridecount: floor(payload_words / STRIDE) - 2.
    ///
    /// matches CUETools.AccurateRip/CDRepair.cs:1127-1129
    stridecount: usize,
    /// CUETools CDRepair.laststride: STRIDE + payload_words % STRIDE.
    ///
    /// matches CUETools.AccurateRip/CDRepair.cs:1127
    laststride: usize,
}

fn cuetools_disc_span(audio: &[i16]) -> Option<CuetoolsDiscSpan> {
    let stride = crate::ctdb_rs::STRIDE;
    if audio.len() < stride.checked_mul(2)? {
        return None;
    }

    // tonepoet's CTDB image shape is [STRIDE synthetic pad] + real decoded PCM
    // + [STRIDE synthetic pad].  CUETools' CDRepair formulas are defined over
    // the real decoded PCM length, not over the synthetic pads.
    let payload_start = stride;
    let payload_len = audio.len().checked_sub(stride.checked_mul(2)?)?;
    let full_payload_rows = payload_len / stride;

    // CUETools excludes the first stride and the final laststride from the LFSR
    // parity workspace.  The first/tail words live only in leadin/leadout context
    // and are introduced by GetSyndrome boundary corrections as offsets move.
    let stridecount = full_payload_rows.checked_sub(2)?;
    let laststride = stride.checked_add(payload_len % stride)?;

    Some(CuetoolsDiscSpan {
        payload_start,
        payload_len,
        stridecount,
        laststride,
    })
}

fn cuetools_payload_from_disc_image(audio: &[i16]) -> Option<&[i16]> {
    let span = cuetools_disc_span(audio)?;
    audio.get(span.payload_start..span.payload_start.checked_add(span.payload_len)?)
}

/// Build a temporary padded image whose protected region is exactly the middle
/// rows that CUETools feeds into its parity LFSR.
///
/// This lets us reuse the validated low-level codec without changing
/// src/ctdb_rs/mod.rs.  The temporary image is:
///
///   [STRIDE zero pad] + payload[STRIDE .. STRIDE + stridecount*STRIDE]
///   + [STRIDE zero pad]
///
/// matches CUETools.AccurateRip/AccurateRip.cs:3099-3105
fn cuetools_middle_span_image_for_parity(audio: &[i16]) -> Option<Vec<i16>> {
    let stride = crate::ctdb_rs::STRIDE;
    let span = cuetools_disc_span(audio)?;
    let payload =
        audio.get(span.payload_start..span.payload_start.checked_add(span.payload_len)?)?;

    let protected_start = stride;
    let protected_len = span.stridecount.checked_mul(stride)?;
    let protected_end = protected_start.checked_add(protected_len)?;
    let protected = payload.get(protected_start..protected_end)?;

    let mut out = Vec::with_capacity(stride.checked_add(protected.len())?.checked_add(stride)?);
    out.extend(std::iter::repeat(0i16).take(stride));
    out.extend_from_slice(protected);
    out.extend(std::iter::repeat(0i16).take(stride));
    Some(out)
}

#[derive(Clone, Debug)]
struct CuetoolsSyndromeContext {
    stride: usize,
    stridecount: usize,
    laststride: usize,
    leadin: Vec<u16>,
    leadout: Vec<u16>,
}

#[inline]
fn i16_as_u16_bits(sample: i16) -> u16 {
    sample as u16
}

/// Reconstructs the side buffers that CUETools AccurateRipVerify.GetSyndrome()
/// reads when an offset crosses the first/last RS column boundary.
///
/// tonepoet's CTDB image is padded as:
///   [STRIDE zeros] + real disc PCM + [STRIDE zeros]
///
/// CUETools.NET does not feed those synthetic padding rows into
/// AccurateRipVerify.leadin / leadout. AccurateRip.cs:3293-3310 fills those
/// arrays from the actual decoded sample stream. Therefore this helper strips
/// tonepoet's artificial CTDB padding before rebuilding the side buffers.
///
/// matches CUETools.AccurateRip/CDRepair.cs:1113-1129
/// matches CUETools.AccurateRip/AccurateRip.cs:2915-2933
/// matches CUETools.AccurateRip/AccurateRip.cs:3293-3310
fn cuetools_build_syndrome_context(audio: &[i16]) -> Option<CuetoolsSyndromeContext> {
    let stride = crate::ctdb_rs::STRIDE;
    let span = cuetools_disc_span(audio)?;
    let payload = cuetools_payload_from_disc_image(audio)?;

    // CUETools allocates at least stride*2 words of lead-in context and fills it
    // from the first two strides of decoded PCM as Write() streams samples.
    let mut leadin = vec![0u16; stride.checked_mul(2)?];
    for (dst, src) in leadin.iter_mut().zip(payload.iter()) {
        *dst = i16_as_u16_bits(*src);
    }

    // CUETools stores leadout in reverse order: leadout[0] is the final word of
    // decoded PCM, leadout[1] the previous word, and so on.
    let mut leadout = vec![0u16; stride.checked_add(span.laststride)?];
    for (dst_index, dst) in leadout.iter_mut().enumerate() {
        if let Some(src_index) = payload.len().checked_sub(1 + dst_index) {
            *dst = i16_as_u16_bits(payload[src_index]);
        }
    }

    Some(CuetoolsSyndromeContext {
        stride,
        stridecount: span.stridecount,
        laststride: span.laststride,
        leadin,
        leadout,
    })
}

#[cfg(test)]
mod cuetools_boundary_context_regression {
    use super::*;

    #[test]
    fn cuetools_context_uses_payload_not_artificial_padding() {
        let stride = crate::ctdb_rs::STRIDE;
        let mut image = Vec::new();
        image.extend(std::iter::repeat(0i16).take(stride));
        image.extend((0..stride * 3).map(|i| ((i as u16).wrapping_add(1)) as i16));
        image.extend(std::iter::repeat(0i16).take(stride));

        let ctx = cuetools_build_syndrome_context(&image).unwrap();

        assert_eq!(ctx.leadin[0], 1);
        assert_eq!(ctx.leadin[stride], (stride as u16).wrapping_add(1));
        assert_eq!(ctx.leadout[0], (stride * 3) as u16);
    }
}

/// One CUETools FindOffset candidate-row diagnostic.
#[derive(Debug, Clone)]
pub struct CtdbSyndromeProbeRow {
    pub offset: i32,
    pub exact_zero: bool,
    pub nonzero_syndrome_words: usize,
    pub delta_or: u16,
    pub errors_found: Option<usize>,
    pub chien_succeeds: bool,
    pub positions: Vec<usize>,
}

/// Diagnostic version of CUETools.AccurateRip/CDRepair.cs::FindOffset.
/// Iterates candidate offsets and reports per-row exact-zero / BM / Chien status
/// without selecting a winner.
pub fn ctdb_probe_entry_offsets_with_parity(
    audio: &[i16],
    parity16: &[Vec<u16>],
    entry: &CtdbEntry,
    offset_window: i32,
) -> Option<Vec<CtdbSyndromeProbeRow>> {
    if entry.stride != crate::ctdb_rs::STRIDE {
        return None;
    }

    let codec = crate::ctdb_rs::CtdbCodec::new();
    let gf = codec.galois();
    let ctx = cuetools_build_syndrome_context(audio)?;
    let (entry_row, npar, _source) = decode_entry_row_cuetools(gf, entry)?;
    if npar == 0 || entry_row.len() < npar {
        return None;
    }

    let stride = crate::ctdb_rs::STRIDE as i32;
    let source_lo = 1 - stride / 2;
    let source_hi = stride / 2 - 1;
    let requested = offset_window.abs();
    let offset_lo = source_lo.max(-requested);
    let offset_hi = source_hi.min(requested);

    let mut out = Vec::with_capacity((offset_hi - offset_lo + 1).max(0) as usize);
    for candidate_offset in offset_lo..=offset_hi {
        let our_row = cuetools_get_syndrome_row(gf, parity16, &ctx, npar, -candidate_offset)?;

        let mut delta = vec![0u16; npar];
        let mut delta_or = 0u16;
        let mut nonzero_syndrome_words = 0usize;
        for i in 0..npar {
            let word = our_row[i] ^ entry_row[i];
            delta[i] = word;
            delta_or |= word;
            if word != 0 {
                nonzero_syndrome_words += 1;
            }
        }

        if delta_or == 0 {
            out.push(CtdbSyndromeProbeRow {
                offset: candidate_offset,
                exact_zero: true,
                nonzero_syndrome_words,
                delta_or,
                errors_found: Some(0),
                chien_succeeds: false,
                positions: Vec::new(),
            });
            continue;
        }

        let mut errors_found = None;
        let mut chien_succeeds = false;
        let mut positions = Vec::new();

        if let Some((sigma, count)) = crate::ctdb_rs::berlekamp_massey(gf, &delta, npar) {
            errors_found = Some(count);
            if count > 0 {
                if let Some(found) =
                    crate::ctdb_rs::chien_search(gf, &sigma, count, ctx.stridecount)
                {
                    chien_succeeds = found.len() == count;
                    positions = found;
                }
            }
        }

        out.push(CtdbSyndromeProbeRow {
            offset: candidate_offset,
            exact_zero: false,
            nonzero_syndrome_words,
            delta_or,
            errors_found,
            chien_succeeds,
            positions,
        });
    }

    Some(out)
}

#[inline]
fn gf_mul_exp(gf: &crate::ctdb_rs::Galois16, value: u16, exp: usize) -> u16 {
    if value == 0 {
        0
    } else {
        gf.mul(value, gf.alpha_pow(exp % crate::ctdb_rs::galois::MAX))
    }
}

#[inline]
fn gf_div_exp(gf: &crate::ctdb_rs::Galois16, value: u16, exp: usize) -> u16 {
    if value == 0 {
        0
    } else {
        gf.div(value, gf.alpha_pow(exp % crate::ctdb_rs::galois::MAX))
    }
}

/// CUETools ParityToSyndrome.Bytes2Syndrome layout:
///   C# ushort[stride, npar], indexed as [row, parity_symbol].
///   Serialized word index is j + i * stride.
///
/// matches CUETools.Parity/Parity2Syndrome.cs:710-735
fn cuetools_bytes_to_syndrome_matrix(
    bytes: &[u8],
    stride: usize,
    npar: usize,
) -> Option<Vec<Vec<u16>>> {
    let required = stride.checked_mul(npar)?.checked_mul(2)?;
    if bytes.len() < required {
        return None;
    }

    let mut out = vec![vec![0u16; npar]; stride];
    for i in 0..npar {
        for j in 0..stride {
            let k = (j + i * stride) * 2;
            out[j][i] = u16::from_le_bytes([bytes[k], bytes[k + 1]]);
        }
    }
    Some(out)
}

/// Direct translation of ParityToSyndrome.Parity2Syndrome's inner transform for
/// one output row. CUETools.GetSyndrome(npar, ...) always transforms from the
/// maxNpar=16 parity workspace, even when out_npar is 8.
///
/// matches CUETools.Parity/Parity2Syndrome.cs:770-823
fn cuetools_parity_row_to_syndrome_row(
    gf: &crate::ctdb_rs::Galois16,
    parity_row: &[u16],
    out_npar: usize,
    source_npar: usize,
) -> Option<Vec<u16>> {
    if out_npar > source_npar || parity_row.len() < source_npar {
        return None;
    }

    let gf_max = crate::ctdb_rs::galois::MAX;
    let mut syn = vec![0u16; out_npar];

    for x1 in 0..source_npar {
        let lo = parity_row[x1];
        if lo == 0 {
            continue;
        }
        let log_lo = gf.log(lo)?;
        for x in 0..out_npar {
            let decrement = ((1 + x1) * x) % gf_max;
            let exp = (log_lo + gf_max - decrement) % gf_max;
            syn[x] ^= gf.alpha_pow(exp);
        }
    }

    Some(syn)
}

/// Direct translation of AccurateRipVerify.GetSyndrome(..., strides: 1, offset)
/// for the single CTDB row used by CDRepairEncode.FindOffset.
///
/// `sample_offset` is CUETools GetSyndrome's `offset` parameter, in stereo
/// sample pairs. CDRepair calls this with `-candidate_offset`.
///
/// matches CUETools.AccurateRip/AccurateRip.cs:2782-2802
/// matches CUETools.AccurateRip/AccurateRip.cs:2805-2848
fn cuetools_get_syndrome_row(
    gf: &crate::ctdb_rs::Galois16,
    parity16: &[Vec<u16>],
    ctx: &CuetoolsSyndromeContext,
    out_npar: usize,
    sample_offset: i32,
) -> Option<Vec<u16>> {
    if out_npar > CUETOOLS_MAX_NPAR || parity16.len() < ctx.stride {
        return None;
    }

    let stride_i64 = ctx.stride as i64;
    let part2 = 0_i64;

    // matches CUETools.AccurateRip/AccurateRip.cs:2798-2802
    let parity2syndrome_word_offset = -2_i64 * sample_offset as i64;
    let y1 = (part2 - parity2syndrome_word_offset).rem_euclid(stride_i64) as usize;

    let mut syn =
        cuetools_parity_row_to_syndrome_row(gf, parity16.get(y1)?, out_npar, CUETOOLS_MAX_NPAR)?;

    // matches CUETools.AccurateRip/AccurateRip.cs:2808
    let offset_words = 2_i64 * sample_offset as i64;
    let part = (part2 + offset_words).rem_euclid(stride_i64);

    // C# first-boundary correction.
    // matches CUETools.AccurateRip/AccurateRip.cs:2810-2827
    if part < offset_words {
        let part_usize = part as usize;
        for i in 0..out_npar {
            let mut syn_i = gf_mul_exp(gf, syn[i], i);
            syn_i ^= *ctx
                .leadout
                .get(ctx.laststride.checked_sub(part_usize + 1)?)?;
            let leadin_index = ctx.stride.checked_add(part_usize)?;
            let exp = (i * ctx.stridecount) % crate::ctdb_rs::galois::MAX;
            syn_i ^= gf_mul_exp(gf, *ctx.leadin.get(leadin_index)?, exp);
            syn[i] = syn_i;
        }
    }

    // C# last-boundary correction.
    // matches CUETools.AccurateRip/AccurateRip.cs:2829-2846
    if part >= stride_i64 + offset_words {
        let part_usize = part as usize;
        for i in 0..out_npar {
            let leadout_index = ctx
                .laststride
                .checked_add(ctx.stride)?
                .checked_sub(part_usize + 1)?;
            let exp = (i * ctx.stridecount) % crate::ctdb_rs::galois::MAX;
            let mut syn_i = syn[i]
                ^ *ctx.leadout.get(leadout_index)?
                ^ gf_mul_exp(gf, *ctx.leadin.get(part_usize)?, exp);
            syn_i = gf_div_exp(gf, syn_i, i);
            syn[i] = syn_i;
        }
    }

    Some(syn)
}

/// Legacy inline `parity=` entries are the old one-row NPAR=8 form.
/// DBEntry.cs converts them with Parity2Syndrome(1, 1, 8, 8, ...).
///
/// matches CUETools.CTDB/DBEntry.cs:441-445
fn cuetools_legacy_inline_parity_to_syndrome_row(
    gf: &crate::ctdb_rs::Galois16,
    bytes: &[u8],
) -> Option<Vec<u16>> {
    let npar = 8;
    if bytes.len() < npar * 2 {
        return None;
    }

    let mut parity_row = vec![0u16; npar];
    for i in 0..npar {
        let k = i * 2;
        parity_row[i] = u16::from_le_bytes([bytes[k], bytes[k + 1]]);
    }

    cuetools_parity_row_to_syndrome_row(gf, &parity_row, npar, npar)
}

/// Decode entry's `syndrome=` (already-syndrome) or legacy `parity=`
/// (raw parity, needs Parity2Syndrome) into a single comparison row.
///
/// matches CUETools.CTDB/DBEntry.cs:441-445
fn decode_entry_row_cuetools(
    gf: &crate::ctdb_rs::Galois16,
    entry: &CtdbEntry,
) -> Option<(Vec<u16>, usize, RsVerifySource)> {
    if let Some(syndrome_b64) = entry.syndrome.as_deref().filter(|s| !s.is_empty()) {
        let npar = (entry.npar as usize).min(CUETOOLS_MAX_NPAR);
        if npar == 0 {
            return None;
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(syndrome_b64)
            .ok()?;
        let matrix = cuetools_bytes_to_syndrome_matrix(&bytes, 1, npar)?;
        return Some((matrix.into_iter().next()?, npar, RsVerifySource::Syndrome));
    }

    if let Some(parity_b64) = entry.parity.as_deref().filter(|s| !s.is_empty()) {
        // DBEntry.cs hard-codes legacy inline parity to NPAR=8.
        let npar = 8;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(parity_b64)
            .ok()?;
        let row = cuetools_legacy_inline_parity_to_syndrome_row(gf, &bytes)?;
        return Some((row, npar, RsVerifySource::InlineParity));
    }

    None
}

/// Source-faithful single-entry FindOffset translation.
///
/// matches CUETools.AccurateRip/CDRepair.cs:1252-1349
fn verify_entry_via_syndrome_fast_path(
    gf: &crate::ctdb_rs::Galois16,
    parity16: &[Vec<u16>],
    ctx: &CuetoolsSyndromeContext,
    entry: &CtdbEntry,
    offset_lo: i32,
    offset_hi: i32,
) -> Option<RsVerifiedMatch> {
    // matches CUETools.AccurateRip/CDRepair.cs:1256-1261
    let (entry_row, npar, source) = decode_entry_row_cuetools(gf, entry)?;
    if entry_row.len() < npar {
        return None;
    }

    let mut best_offset = 0_i32;
    let mut best_offset_errors = npar / 2;

    // matches CUETools.AccurateRip/CDRepair.cs:1285
    for candidate_offset in offset_lo..=offset_hi {
        // matches CUETools.AccurateRip/CDRepair.cs:1288
        let our_row = cuetools_get_syndrome_row(gf, parity16, ctx, npar, -candidate_offset)?;

        // matches CUETools.AccurateRip/CDRepair.cs:1290-1300
        let mut delta = vec![0u16; npar];
        for i in 0..npar {
            delta[i] = our_row[i] ^ entry_row[i];
        }

        // C# exact-zero branch.
        // matches CUETools.AccurateRip/CDRepair.cs:1293-1312
        if delta.iter().all(|&word| word == 0) {
            return Some(RsVerifiedMatch {
                entry: entry.clone(),
                offset: candidate_offset,
                confidence: entry.confidence,
                npar,
                source,
                column0_errors: 0,
            });
        }

        // matches CUETools.AccurateRip/CDRepair.cs:1316-1324
        let Some((sigma, errors_found)) = crate::ctdb_rs::berlekamp_massey(gf, &delta, npar) else {
            continue;
        };
        if errors_found > 0 && errors_found < best_offset_errors {
            if let Some(positions) =
                crate::ctdb_rs::chien_search(gf, &sigma, errors_found, ctx.stridecount)
            {
                if positions.len() == errors_found {
                    best_offset_errors = errors_found;
                    best_offset = candidate_offset;
                }
            }
        }
    }

    // matches CUETools.AccurateRip/CDRepair.cs:1332-1348
    if best_offset_errors < npar / 2 {
        Some(RsVerifiedMatch {
            entry: entry.clone(),
            offset: best_offset,
            confidence: entry.confidence,
            npar,
            source,
            column0_errors: best_offset_errors,
        })
    } else {
        None
    }
}

// Helpers used by verify_ctdb / verify_ctdb_single_image (carried over from v2).

fn has_parity_material(entry: &CtdbEntry) -> bool {
    matches!(entry.has_parity.as_deref(), Some(url) if !url.is_empty())
}

fn verified_status(status: &CtdbTrackStatus) -> bool {
    matches!(
        status,
        CtdbTrackStatus::Verified | CtdbTrackStatus::VerifiedRs
    )
}

fn assemble_ctdb_disc_image_from_tracks(decoded_tracks: &[Option<Vec<i16>>]) -> Option<Vec<i16>> {
    if decoded_tracks.iter().any(|track| track.is_none()) {
        return None;
    }

    let stride = crate::ctdb_rs::STRIDE;
    let total_track_i16: usize = decoded_tracks
        .iter()
        .filter_map(|track| track.as_ref())
        .map(|track| track.len())
        .sum();

    let mut image = Vec::with_capacity(stride + total_track_i16 + stride);
    image.extend(std::iter::repeat(0i16).take(stride));
    for track in decoded_tracks.iter().filter_map(|track| track.as_ref()) {
        image.extend_from_slice(track);
    }
    image.extend(std::iter::repeat(0i16).take(stride));
    Some(image)
}

fn assemble_ctdb_disc_image_from_audio(raw_i16: &[i16]) -> Vec<i16> {
    let stride = crate::ctdb_rs::STRIDE;
    let mut image = Vec::with_capacity(stride + raw_i16.len() + stride);
    image.extend(std::iter::repeat(0i16).take(stride));
    image.extend_from_slice(raw_i16);
    image.extend(std::iter::repeat(0i16).take(stride));
    image
}

/// Source-faithful CTDB syndrome verification (CUETools FindOffset).
///
/// Computes the maxNpar=16 parity workspace once, derives 8/16-symbol
/// comparison rows from it as `AccurateRipVerify.GetSyndrome` does, and
/// scans the CUETools offset window. Returns the highest-confidence entry
/// that verifies (exact zero or BM/Chien-correctable column 0).
///
/// matches CUETools.AccurateRip/AccurateRip.cs:2782-2848
/// matches CUETools.AccurateRip/CDRepair.cs:1252-1349
pub async fn verify_disc_via_rs(
    audio: &[i16],
    entries: &[CtdbEntry],
    offset_window: i32,
) -> Option<RsVerifiedMatch> {
    let audio = audio.to_vec();
    let entries = entries.to_vec();
    tokio::task::spawn_blocking(move || {
        verify_disc_via_rs_blocking(&audio, &entries, offset_window)
    })
    .await
    .ok()
    .flatten()
}

/// Variant of `verify_disc_via_rs` that takes a precomputed parity matrix.
/// The orchestrator can populate `parity16` from the parity cache to skip
/// the (~20 sec) `compute_parity_matrix_from_audio` step.
pub async fn verify_disc_via_rs_with_parity_matrix(
    audio: &[i16],
    parity16: Vec<Vec<u16>>,
    entries: &[CtdbEntry],
    offset_window: i32,
) -> Option<RsVerifiedMatch> {
    let audio = audio.to_vec();
    let entries = entries.to_vec();
    tokio::task::spawn_blocking(move || {
        verify_disc_via_rs_blocking_with_parity(&audio, &parity16, &entries, offset_window)
    })
    .await
    .ok()
    .flatten()
}

fn verify_disc_via_rs_blocking(
    audio: &[i16],
    entries: &[CtdbEntry],
    offset_window: i32,
) -> Option<RsVerifiedMatch> {
    let parity16 = compute_audio_parity16(audio)?;
    verify_disc_via_rs_blocking_with_parity(audio, &parity16, entries, offset_window)
}

fn verify_disc_via_rs_blocking_with_parity(
    audio: &[i16],
    parity16: &[Vec<u16>],
    entries: &[CtdbEntry],
    offset_window: i32,
) -> Option<RsVerifiedMatch> {
    let codec = crate::ctdb_rs::CtdbCodec::new();
    let gf = codec.galois();
    let ctx = cuetools_build_syndrome_context(audio)?;

    let stride = crate::ctdb_rs::STRIDE as i32;
    let source_lo = 1 - stride / 2;
    let source_hi = stride / 2 - 1;
    let requested_lo = -offset_window.abs();
    let requested_hi = offset_window.abs();
    let offset_lo = source_lo.max(requested_lo);
    let offset_hi = source_hi.min(requested_hi);

    let mut best_match: Option<RsVerifiedMatch> = None;

    for entry in entries {
        let candidate =
            verify_entry_via_syndrome_fast_path(gf, parity16, &ctx, entry, offset_lo, offset_hi);

        if let Some(candidate) = candidate {
            let replace = match best_match.as_ref() {
                None => true,
                Some(current) => {
                    candidate.confidence > current.confidence
                        || (candidate.confidence == current.confidence
                            && candidate.column0_errors < current.column0_errors)
                }
            };
            if replace {
                log::info!(
                    "CTDB RS: entry {} confidence {} verified via {:?} at offset {:+} (column0_errors={})",
                    candidate.entry.id,
                    candidate.confidence,
                    candidate.source,
                    candidate.offset,
                    candidate.column0_errors,
                );
                best_match = Some(candidate);
            }
        }
    }

    best_match
}

#[cfg(test)]
mod cuetools_translation_fixtures {
    use super::*;

    #[test]
    fn parity2syndrome_single_row_fixture() {
        let gf = crate::ctdb_rs::Galois16::new();
        let parity_row = [0x1234, 0xabcd, 0x0000, 0xbeef];
        let got = cuetools_parity_row_to_syndrome_row(&gf, &parity_row, 4, 4).unwrap();
        assert_eq!(got, vec![0x0716, 0x3907, 0xcf58, 0xcf08]);
    }

    #[test]
    fn bytes2syndrome_layout_fixture() {
        let mut bytes = Vec::new();
        for word in [0x1111u16, 0x3333, 0x2222, 0x4444] {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        let got = cuetools_bytes_to_syndrome_matrix(&bytes, 2, 2).unwrap();
        assert_eq!(got, vec![vec![0x1111, 0x2222], vec![0x3333, 0x4444]]);
    }

    #[test]
    fn cuetools_context_uses_middle_span_stridecount_and_real_payload_edges() {
        let stride = crate::ctdb_rs::STRIDE;
        let payload_rows = 5usize;
        let rem = 1176usize;
        let payload_len = payload_rows * stride + rem;
        let mut audio = vec![0i16; stride + payload_len + stride];

        let payload_start = stride;
        audio[payload_start] = 0x1111i16;
        audio[payload_start + stride] = 0x2222i16;
        audio[payload_start + payload_len - 1] = 0x3333i16;

        let ctx = cuetools_build_syndrome_context(&audio).unwrap();
        assert_eq!(ctx.stridecount, payload_rows - 2);
        assert_eq!(ctx.laststride, stride + rem);
        assert_eq!(ctx.leadin[0], 0x1111);
        assert_eq!(ctx.leadin[stride], 0x2222);
        assert_eq!(ctx.leadout[0], 0x3333);
    }

    #[test]
    fn cuetools_parity_excludes_first_stride_and_final_laststride() {
        let stride = crate::ctdb_rs::STRIDE;
        let payload_rows = 5usize;
        let rem = 1176usize;
        let payload_len = payload_rows * stride + rem;
        let payload_start = stride;

        let mut audio = vec![0i16; stride + payload_len + stride];

        // These are outside CUETools' LFSR-protected middle span: row 0 and the
        // final laststride.  They should not affect compute_audio_parity16();
        // GetSyndrome boundary corrections account for them separately.
        audio[payload_start + 123] = 0x1357i16;
        audio[payload_start + (payload_rows - 1) * stride + 456] = 0x2468i16;
        audio[payload_start + payload_rows * stride + 17] = 0x55aai16;

        let parity = compute_audio_parity16(&audio).unwrap();
        assert!(
            parity.iter().all(|col| col.iter().all(|&word| word == 0)),
            "first stride / final laststride leaked into the CUETools parity workspace"
        );

        // This word is in protected middle row 1, so it must affect the parity.
        audio[payload_start + stride + 789] = 0x7b7bi16;
        let parity = compute_audio_parity16(&audio).unwrap();
        assert!(
            parity.iter().any(|col| col.iter().any(|&word| word != 0)),
            "middle protected rows were not fed into the CUETools parity workspace"
        );
    }
}

// CUETools-source-faithful repair flow (round 2). See README in
// ctdb_cuetools_repair_translation_patch.zip for source citations.
include!("ctdb_cuetools_repair.rs");

// ── Verification orchestrator ───────────────────────────────────────

/// Verify an album against the CUETools Database.
///
/// Builds a TOC, queries CTDB, decodes each track, computes CRC32,
/// and uses Reed-Solomon parity as the album-level verification signal.
///
/// `cache_key` and `cached_parity` are the parity cache hooks: pass
/// `Some(key)` when the caller wants the resulting parity matrix written
/// back to the cache, and pass `Some(parity)` when the caller already has
/// a cached matrix (skips the ~20 sec parity computation). On a cache
/// miss, the result's `parity_cache_write` is populated for the caller
/// to persist via `Database::store_ctdb_parity`.
pub async fn verify_ctdb(
    paths: &[PathBuf],
    sample_counts: &[u64],
    sample_rate: u32,
    cache_key: Option<String>,
    cached_parity: Option<Vec<Vec<u16>>>,
) -> CtdbVerifyResult {
    let n = paths.len();
    if n == 0 {
        return CtdbVerifyResult {
            tracks: Vec::new(),
            toc: String::new(),
            npar: None,
            stride: None,
            parity_url: None,
            parity_cache_write: None,
        };
    }

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
                tracks: (0..n)
                    .map(|i| CtdbTrackResult {
                        path: paths[i].clone(),
                        track_number: (i + 1) as u32,
                        status: CtdbTrackStatus::NoDiscInDatabase,
                        confidence: None,
                        computed_crc32: 0,
                        expected_crc32: None,
                        has_parity: false,
                    })
                    .collect(),
                toc,
                npar: None,
                stride: None,
                parity_url: None,
                parity_cache_write: None,
            };
        }
        Err(e) => {
            return CtdbVerifyResult {
                tracks: (0..n)
                    .map(|i| CtdbTrackResult {
                        path: paths[i].clone(),
                        track_number: (i + 1) as u32,
                        status: CtdbTrackStatus::Error(e.clone()),
                        confidence: None,
                        computed_crc32: 0,
                        expected_crc32: None,
                        has_parity: false,
                    })
                    .collect(),
                toc,
                npar: None,
                stride: None,
                parity_url: None,
                parity_cache_write: None,
            };
        }
    };

    let highest_conf_entry = match db_response.entries.iter().max_by_key(|e| e.confidence) {
        Some(entry) => entry,
        None => {
            return CtdbVerifyResult {
                tracks: (0..n)
                    .map(|i| CtdbTrackResult {
                        path: paths[i].clone(),
                        track_number: (i + 1) as u32,
                        status: CtdbTrackStatus::NoDiscInDatabase,
                        confidence: None,
                        computed_crc32: 0,
                        expected_crc32: None,
                        has_parity: false,
                    })
                    .collect(),
                toc,
                npar: None,
                stride: None,
                parity_url: None,
                parity_cache_write: None,
            };
        }
    };

    // Decode each track and retain PCM so RS verification can operate on the
    // whole CTDB disc image.
    let mut decode_handles = Vec::with_capacity(n);
    for path in paths.iter() {
        let path_clone = path.clone();
        let handle = tokio::task::spawn_blocking(move || {
            super::accuraterip::decode_track_to_raw_i16(&path_clone)
                .or_else(|_| super::accuraterip::decode_to_raw_i16_wvunpack(&path_clone))
        });
        decode_handles.push(handle);
    }

    let mut decoded_tracks: Vec<Option<Vec<i16>>> = vec![None; n];
    let mut decode_errors: Vec<Option<String>> = vec![None; n];

    for (i, handle) in decode_handles.into_iter().enumerate() {
        match handle.await {
            Ok(Ok(data)) => {
                decoded_tracks[i] = Some(data);
            }
            Ok(Err(e)) => {
                decode_errors[i] = Some(e);
            }
            Err(e) => {
                decode_errors[i] = Some(format!("decode failed: {}", e));
            }
        }
    }

    // Cache-aware parity flow: use cached parity if supplied, else compute
    // fresh and stash for the caller to write back if cache_key is Some.
    let (rs_match, parity_cache_write) = match assemble_ctdb_disc_image_from_tracks(&decoded_tracks)
    {
        Some(image) => {
            // Resolve parity: from cache, or freshly computed.
            let parity_and_write: Option<(Vec<Vec<u16>>, Option<(String, Vec<Vec<u16>>)>)> =
                match cached_parity {
                    Some(p) => {
                        log::info!("CTDB RS: using cached parity matrix");
                        Some((p, None))
                    }
                    None => {
                        let image_for_parity = image.clone();
                        let computed = tokio::task::spawn_blocking(move || {
                            compute_audio_parity16(&image_for_parity)
                        })
                        .await
                        .ok()
                        .flatten();
                        computed.map(|p| {
                            let cache_write = cache_key.map(|k| (k, p.clone()));
                            (p, cache_write)
                        })
                    }
                };

            match parity_and_write {
                Some((parity, cache_write)) => {
                    let m = verify_disc_via_rs_with_parity_matrix(
                        &image,
                        parity,
                        &db_response.entries,
                        CTDB_RS_OFFSET_WINDOW_SAMPLES,
                    )
                    .await;
                    (m, cache_write)
                }
                None => {
                    log::warn!(
                        "CTDB RS: failed to compute parity matrix; skipping RS verification"
                    );
                    (None, None)
                }
            }
        }
        None => (None, None),
    };

    let entry = rs_match
        .as_ref()
        .map(|m| &m.entry)
        .unwrap_or(highest_conf_entry);

    if let Some(m) = &rs_match {
        log::info!(
            "CTDB RS verified entry {} confidence {} via {:?} at sample offset {:+}",
            m.entry.id,
            m.entry.confidence,
            m.source,
            m.offset,
        );
    } else {
        log::info!(
            "CTDB RS verification did not find a match; falling back to highest-confidence entry {} for CRC diagnostics",
            highest_conf_entry.id,
        );
    }

    let result_npar = Some(entry.npar);
    let result_stride = Some(entry.stride);
    let result_parity_url = entry.has_parity.clone();

    // Compute the last-track suffix skip from total disc samples.
    let total_disc_samples: u64 = sample_counts.iter().sum();
    let suffix_skip = compute_suffix_skip(total_disc_samples);

    let mut results = Vec::with_capacity(n);
    for i in 0..n {
        let decoded = match decoded_tracks[i].as_ref() {
            Some(data) => data,
            None => {
                results.push(CtdbTrackResult {
                    path: paths[i].clone(),
                    track_number: (i + 1) as u32,
                    status: CtdbTrackStatus::Error(
                        decode_errors[i]
                            .clone()
                            .unwrap_or_else(|| "decode failed".to_string()),
                    ),
                    confidence: None,
                    computed_crc32: 0,
                    expected_crc32: None,
                    has_parity: has_parity_material(entry),
                });
                continue;
            }
        };

        let is_first = i == 0;
        let is_last = i == n - 1;
        let computed = compute_track_crc32(decoded, is_first, is_last, suffix_skip);
        let db_crc = entry.track_crcs.get(i).copied();

        let status = match db_crc {
            Some(expected) if expected == computed => CtdbTrackStatus::Verified,
            Some(_) if rs_match.is_some() => CtdbTrackStatus::VerifiedRs,
            Some(_) => CtdbTrackStatus::Mismatch,
            None => CtdbTrackStatus::Error("track not in CTDB entry".to_string()),
        };

        results.push(CtdbTrackResult {
            path: paths[i].clone(),
            track_number: (i + 1) as u32,
            status,
            confidence: Some(entry.confidence),
            computed_crc32: computed,
            expected_crc32: db_crc,
            has_parity: has_parity_material(entry),
        });
    }

    CtdbVerifyResult {
        tracks: results,
        toc,
        npar: result_npar,
        stride: result_stride,
        parity_url: result_parity_url,
        parity_cache_write,
    }
}

/// Format a summary string for CTDB verification results.
pub fn format_ctdb_summary(result: &CtdbVerifyResult) -> String {
    let total = result.tracks.len();
    let verified = result
        .tracks
        .iter()
        .filter(|t| verified_status(&t.status))
        .count();
    let rs_verified = result
        .tracks
        .iter()
        .filter(|t| t.status == CtdbTrackStatus::VerifiedRs)
        .count();

    if result
        .tracks
        .iter()
        .any(|t| t.status == CtdbTrackStatus::NoDiscInDatabase)
    {
        return "Disc not in CUETools database".to_string();
    }

    if verified == 0 {
        return format!("0/{} tracks verified", total);
    }

    let max_conf = result
        .tracks
        .iter()
        .filter_map(|t| t.confidence)
        .max()
        .unwrap_or(0);

    let has_parity = result.tracks.iter().any(|t| t.has_parity);
    let parity_str = if has_parity { ", parity available" } else { "" };
    let rs_str = if rs_verified > 0 {
        ", RS verified with CRC differences"
    } else {
        ""
    };

    format!(
        "{}/{} verified, confidence {}{}{}",
        verified, total, max_conf, parity_str, rs_str,
    )
}

/// Verify a single-image CUE album against CTDB.
///
/// Decodes the full image, splits by CUE boundaries, computes per-track
/// CRC32, and uses Reed-Solomon parity as the album-level verification signal.
///
/// `cache_key` and `cached_parity` mirror `verify_ctdb`'s parity cache hooks.
pub async fn verify_ctdb_single_image(
    info: &super::cue_parser::SingleImageInfo,
    cache_key: Option<String>,
    cached_parity: Option<Vec<Vec<u16>>>,
) -> CtdbVerifyResult {
    let n = info.track_boundaries.len();

    // Build TOC from CUE INDEX timestamps.
    let toc = {
        let toc_sectors =
            super::accuraterip::find_toc_offsets(info.cue_path.parent().unwrap_or(Path::new(".")));
        if let Some(ref sectors) = toc_sectors {
            if sectors.len() == n + 1 {
                build_ctdb_toc(sectors)
            } else {
                let sample_counts: Vec<u64> = info
                    .track_boundaries
                    .iter()
                    .map(|&(_, count)| count)
                    .collect();
                build_ctdb_toc_from_samples(&sample_counts, info.sample_rate)
            }
        } else {
            let sample_counts: Vec<u64> = info
                .track_boundaries
                .iter()
                .map(|&(_, count)| count)
                .collect();
            build_ctdb_toc_from_samples(&sample_counts, info.sample_rate)
        }
    };

    // Query CTDB.
    let db_response = match query_ctdb(&toc).await {
        Ok(Some(resp)) => resp,
        Ok(None) => {
            return CtdbVerifyResult {
                tracks: (0..n)
                    .map(|i| CtdbTrackResult {
                        path: info.audio_path.clone(),
                        track_number: (i + 1) as u32,
                        status: CtdbTrackStatus::NoDiscInDatabase,
                        confidence: None,
                        computed_crc32: 0,
                        expected_crc32: None,
                        has_parity: false,
                    })
                    .collect(),
                toc,
                npar: None,
                stride: None,
                parity_url: None,
                parity_cache_write: None,
            };
        }
        Err(e) => {
            return CtdbVerifyResult {
                tracks: (0..n)
                    .map(|i| CtdbTrackResult {
                        path: info.audio_path.clone(),
                        track_number: (i + 1) as u32,
                        status: CtdbTrackStatus::Error(e.clone()),
                        confidence: None,
                        computed_crc32: 0,
                        expected_crc32: None,
                        has_parity: false,
                    })
                    .collect(),
                toc,
                npar: None,
                stride: None,
                parity_url: None,
                parity_cache_write: None,
            };
        }
    };

    let highest_conf_entry = match db_response.entries.iter().max_by_key(|e| e.confidence) {
        Some(e) => e,
        None => {
            return CtdbVerifyResult {
                tracks: (0..n)
                    .map(|i| CtdbTrackResult {
                        path: info.audio_path.clone(),
                        track_number: (i + 1) as u32,
                        status: CtdbTrackStatus::NoDiscInDatabase,
                        confidence: None,
                        computed_crc32: 0,
                        expected_crc32: None,
                        has_parity: false,
                    })
                    .collect(),
                toc,
                npar: None,
                stride: None,
                parity_url: None,
                parity_cache_write: None,
            };
        }
    };

    // Decode the full image. Try ffmpeg, fall back to wvunpack.
    let audio_path = info.audio_path.clone();
    let raw_result = tokio::task::spawn_blocking(move || {
        super::accuraterip::decode_track_to_raw_i16(&audio_path)
            .or_else(|_| super::accuraterip::decode_to_raw_i16_wvunpack(&audio_path))
    })
    .await;

    let raw_i16 = match raw_result {
        Ok(Ok(data)) => data,
        Ok(Err(e)) => {
            return CtdbVerifyResult {
                tracks: (0..n)
                    .map(|i| CtdbTrackResult {
                        path: info.audio_path.clone(),
                        track_number: (i + 1) as u32,
                        status: CtdbTrackStatus::Error(e.clone()),
                        confidence: None,
                        computed_crc32: 0,
                        expected_crc32: None,
                        has_parity: false,
                    })
                    .collect(),
                toc,
                npar: None,
                stride: None,
                parity_url: None,
                parity_cache_write: None,
            };
        }
        Err(e) => {
            return CtdbVerifyResult {
                tracks: (0..n)
                    .map(|i| CtdbTrackResult {
                        path: info.audio_path.clone(),
                        track_number: (i + 1) as u32,
                        status: CtdbTrackStatus::Error(format!("decode failed: {}", e)),
                        confidence: None,
                        computed_crc32: 0,
                        expected_crc32: None,
                        has_parity: false,
                    })
                    .collect(),
                toc,
                npar: None,
                stride: None,
                parity_url: None,
                parity_cache_write: None,
            };
        }
    };

    let image = assemble_ctdb_disc_image_from_audio(&raw_i16);

    // Cache-aware parity flow (mirrors verify_ctdb).
    let parity_and_write: Option<(Vec<Vec<u16>>, Option<(String, Vec<Vec<u16>>)>)> =
        match cached_parity {
            Some(p) => {
                log::info!("CTDB RS: using cached parity matrix");
                Some((p, None))
            }
            None => {
                let image_for_parity = image.clone();
                let computed =
                    tokio::task::spawn_blocking(move || compute_audio_parity16(&image_for_parity))
                        .await
                        .ok()
                        .flatten();
                computed.map(|p| {
                    let cache_write = cache_key.map(|k| (k, p.clone()));
                    (p, cache_write)
                })
            }
        };

    let (rs_match, parity_cache_write) = match parity_and_write {
        Some((parity, cache_write)) => {
            let m = verify_disc_via_rs_with_parity_matrix(
                &image,
                parity,
                &db_response.entries,
                CTDB_RS_OFFSET_WINDOW_SAMPLES,
            )
            .await;
            (m, cache_write)
        }
        None => {
            log::warn!("CTDB RS: failed to compute parity matrix; skipping RS verification");
            (None, None)
        }
    };
    drop(image);

    let entry = rs_match
        .as_ref()
        .map(|m| &m.entry)
        .unwrap_or(highest_conf_entry);

    if let Some(m) = &rs_match {
        log::info!(
            "CTDB RS verified entry {} confidence {} via {:?} at sample offset {:+}",
            m.entry.id,
            m.entry.confidence,
            m.source,
            m.offset,
        );
    } else {
        log::info!(
            "CTDB RS verification did not find a match; falling back to highest-confidence entry {} for CRC diagnostics",
            highest_conf_entry.id,
        );
    }

    let result_npar = Some(entry.npar);
    let result_stride = Some(entry.stride);
    let result_parity_url = entry.has_parity.clone();

    // Compute per-track CRC32 from segments.
    let mut results = Vec::with_capacity(n);
    let suffix_skip = compute_suffix_skip(info.total_samples);

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
                expected_crc32: None,
                has_parity: has_parity_material(entry),
            });
            continue;
        }

        let track_audio = &raw_i16[start..end];
        let is_first = i == 0;
        let is_last = i == n - 1;
        let computed = compute_track_crc32(track_audio, is_first, is_last, suffix_skip);
        let db_crc = entry.track_crcs.get(i).copied();

        let status = match db_crc {
            Some(expected) if expected == computed => CtdbTrackStatus::Verified,
            Some(_) if rs_match.is_some() => CtdbTrackStatus::VerifiedRs,
            Some(_) => CtdbTrackStatus::Mismatch,
            None => CtdbTrackStatus::Error("track not in CTDB entry".to_string()),
        };

        results.push(CtdbTrackResult {
            path: info.audio_path.clone(),
            track_number: (i + 1) as u32,
            status,
            confidence: Some(entry.confidence),
            computed_crc32: computed,
            expected_crc32: db_crc,
            has_parity: has_parity_material(entry),
        });
    }

    CtdbVerifyResult {
        tracks: results,
        toc,
        npar: result_npar,
        stride: result_stride,
        parity_url: result_parity_url,
        parity_cache_write,
    }
}

// ── Parity download ─────────────────────────────────────────────────

/// Download parity data from the CTDB `hasparity` URL.
async fn download_parity(url: &str) -> Result<Vec<u8>, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("HTTP client error: {}", e))?;

    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("Parity download failed: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("Parity download HTTP {}", resp.status()));
    }

    resp.bytes()
        .await
        .map(|b| b.to_vec())
        .map_err(|e| format!("Parity read error: {}", e))
}

// ── Repair orchestrator ─────────────────────────────────────────────

/// Repair an album using CTDB Reed-Solomon parity data.
///
/// Steps:
/// 1. Download parity from the CTDB `hasparity` URL.
/// 2. Decode all tracks to raw i16 PCM.
/// 3. Assemble a disc image: [STRIDE zeros] + [track1] + ... + [STRIDE zeros].
/// 4. Run `CtdbCodec::repair()` on the image.
/// 5. Split repaired image back into per-track segments.
/// 6. Encode each track to its original format in a temp directory.
/// 7. Copy metadata from originals.
/// 8. Verify repaired files via CTDB CRC32.
/// 9. Replace originals with backup/restore pattern.
pub async fn repair_album(
    paths: &[PathBuf],
    parity_url: &str,
    npar: usize,
    offset: i32,
    expected_crcs: &[u32],
    tx: mpsc::Sender<AppMessage>,
) -> Result<String, String> {
    let n = paths.len();
    if n == 0 {
        return Err("No tracks to repair".to_string());
    }

    // 1. Download parity.
    let _ = tx
        .send(AppMessage::StatusMessage(
            "CTDB repair: downloading parity...".into(),
        ))
        .await;
    let parity_bytes = download_parity(parity_url).await?;
    log::info!(
        "CTDB repair: downloaded {} bytes of parity",
        parity_bytes.len()
    );

    // 2. Decode all tracks to raw i16 (parallel decode, sequential concat).
    let _ = tx
        .send(AppMessage::StatusMessage(format!(
            "CTDB repair: decoding {} tracks...",
            n
        )))
        .await;

    let mut decode_handles = Vec::with_capacity(n);
    for path in paths.iter() {
        let p = path.clone();
        decode_handles.push(tokio::task::spawn_blocking(move || {
            super::accuraterip::decode_track_to_raw_i16(&p)
                .or_else(|_| super::accuraterip::decode_to_raw_i16_wvunpack(&p))
        }));
    }

    let mut track_pcm: Vec<Vec<i16>> = Vec::with_capacity(n);
    for (i, handle) in decode_handles.into_iter().enumerate() {
        let data = handle
            .await
            .map_err(|e| format!("Decode task {} failed: {}", i + 1, e))?
            .map_err(|e| format!("Track {} decode error: {}", i + 1, e))?;
        track_pcm.push(data);
    }

    let track_lengths: Vec<usize> = track_pcm.iter().map(|t| t.len()).collect();

    // 3. Assemble disc image with STRIDE leadin/leadout.
    let stride = crate::ctdb_rs::STRIDE;
    let total_track_i16: usize = track_lengths.iter().sum();
    let image_len = stride + total_track_i16 + stride;
    let mut image: Vec<i16> = Vec::with_capacity(image_len);
    image.extend(std::iter::repeat(0i16).take(stride)); // leadin
    for track in &track_pcm {
        image.extend_from_slice(track);
    }
    image.extend(std::iter::repeat(0i16).take(stride)); // leadout
    drop(track_pcm); // free memory

    // 4. Run RS repair.
    let _ = tx
        .send(AppMessage::StatusMessage(
            "CTDB repair: running Reed-Solomon repair...".into(),
        ))
        .await;

    let parity_clone = parity_bytes;
    let repair_result = tokio::task::spawn_blocking(move || {
        repair_disc_via_rs_with_npar_blocking(&mut image, &parity_clone, npar, offset)
            .map(|r| (r, image))
    })
    .await
    .map_err(|e| format!("Repair task failed: {}", e))?;

    let (result, repaired_image) = match repair_result {
        Ok((r, img)) => (r, img),
        Err(e) => return Err(format!("Repair failed: {}", e)),
    };

    log::info!(
        "CTDB repair: corrected {} samples at {} positions",
        result.corrected_samples,
        result.error_positions.len()
    );

    if result.corrected_samples == 0 {
        return Ok("No errors found — repair not needed".to_string());
    }

    // 5. Split repaired image back into per-track segments.
    let mut repaired_tracks: Vec<Vec<i16>> = Vec::with_capacity(n);
    let mut img_offset = stride; // skip leadin
    for &len in &track_lengths {
        let end = (img_offset + len).min(repaired_image.len());
        repaired_tracks.push(repaired_image[img_offset..end].to_vec());
        img_offset += len;
    }
    drop(repaired_image); // free memory

    // 6. Encode each track to a temp directory.
    let pid = std::process::id();
    let tmp_dir = PathBuf::from(format!("/tmp/tonepoet-ctdb-repair-{}", pid));
    std::fs::create_dir_all(&tmp_dir).map_err(|e| format!("Failed to create temp dir: {}", e))?;

    let _ = tx
        .send(AppMessage::StatusMessage(format!(
            "CTDB repair: re-encoding {} tracks...",
            n
        )))
        .await;

    for (i, (track_data, original_path)) in repaired_tracks.iter().zip(paths.iter()).enumerate() {
        let out_path = tmp_dir.join(original_path.file_name().unwrap_or_default());

        super::accuraterip::encode_corrected_track(track_data, &out_path, original_path)
            .await
            .map_err(|e| {
                let _ = std::fs::remove_dir_all(&tmp_dir);
                format!("Track {} encode failed: {}", i + 1, e)
            })?;

        super::accuraterip::copy_metadata(original_path, &out_path)
            .await
            .map_err(|e| {
                let _ = std::fs::remove_dir_all(&tmp_dir);
                format!("Track {} metadata copy failed: {}", i + 1, e)
            })?;
    }

    // 7. Verify repaired files via CTDB CRC32.
    let _ = tx
        .send(AppMessage::StatusMessage(
            "CTDB repair: verifying repaired files...".into(),
        ))
        .await;

    let total_disc_samples = total_track_i16 as u64 / 2; // i16 count to stereo pairs
    let suffix_skip = compute_suffix_skip(total_disc_samples);

    for (i, track_data) in repaired_tracks.iter().enumerate() {
        let is_first = i == 0;
        let is_last = i == n - 1;
        let computed = compute_track_crc32(track_data, is_first, is_last, suffix_skip);

        if let Some(&expected) = expected_crcs.get(i) {
            if computed != expected {
                let _ = std::fs::remove_dir_all(&tmp_dir);
                return Err(format!(
                    "Track {} verification failed: repaired CRC {:08X} != expected {:08X}. \
                     Originals not modified.",
                    i + 1,
                    computed,
                    expected,
                ));
            }
            log::info!(
                "CTDB repair: track {} verified CRC32 = {:08X} ✓",
                i + 1,
                computed,
            );
        } else {
            log::warn!(
                "CTDB repair: track {} has no expected CRC, skipping verification (CRC = {:08X})",
                i + 1,
                computed,
            );
        }
    }

    // 8. Replace originals with backup/restore pattern.
    let _ = tx
        .send(AppMessage::StatusMessage(
            "CTDB repair: replacing originals...".into(),
        ))
        .await;

    // Phase 1: backup originals to .bak.
    let mut backed_up: Vec<PathBuf> = Vec::with_capacity(n);
    for path in paths {
        let orig_ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let bak = path.with_extension(format!("{}.bak", orig_ext));
        if let Err(e) = std::fs::copy(path, &bak) {
            for (j, bak_path) in backed_up.iter().enumerate() {
                let _ = std::fs::copy(bak_path, &paths[j]);
                let _ = std::fs::remove_file(bak_path);
            }
            let _ = std::fs::remove_dir_all(&tmp_dir);
            return Err(format!("Backup failed for {}: {}", path.display(), e));
        }
        backed_up.push(bak);
    }

    // Phase 2: copy repaired files over originals.
    for path in paths.iter() {
        let tmp_path = tmp_dir.join(path.file_name().unwrap_or_default());
        if let Err(e) = std::fs::copy(&tmp_path, path) {
            for (j, orig_path) in paths.iter().enumerate() {
                let _ = std::fs::copy(&backed_up[j], orig_path);
            }
            for bak in &backed_up {
                let _ = std::fs::remove_file(bak);
            }
            let _ = std::fs::remove_dir_all(&tmp_dir);
            return Err(format!("Replace failed for {}: {}", path.display(), e));
        }
    }

    // Phase 3: remove backups and temp dir.
    for bak in &backed_up {
        let _ = std::fs::remove_file(bak);
    }
    let _ = std::fs::remove_dir_all(&tmp_dir);

    Ok(format!(
        "CTDB repair: corrected {} samples in {} positions across {} tracks",
        result.corrected_samples,
        result.error_positions.len(),
        n,
    ))
}

/// Repair a single-image CUE album using CTDB Reed-Solomon parity data.
///
/// Single-image albums store all tracks in one audio file with track
/// boundaries defined by the CUE INDEX 01 timestamps. The repair operates
/// on the whole image at once: decode once, repair the entire disc image,
/// re-encode as one file. Per-track CRC32 verification still uses the CUE
/// boundaries to slice the repaired audio for comparison.
pub async fn repair_single_image(
    info: &super::cue_parser::SingleImageInfo,
    parity_url: &str,
    npar: usize,
    offset: i32,
    expected_crcs: &[u32],
    tx: mpsc::Sender<AppMessage>,
) -> Result<String, String> {
    let n = info.track_boundaries.len();
    if n == 0 {
        return Err("Single-image CUE has no tracks".to_string());
    }
    if expected_crcs.len() != n {
        return Err(format!(
            "Expected CRC count ({}) doesn't match track count ({})",
            expected_crcs.len(),
            n,
        ));
    }

    // 1. Download parity.
    let _ = tx
        .send(AppMessage::StatusMessage(
            "CTDB repair: downloading parity...".into(),
        ))
        .await;
    let parity_bytes = download_parity(parity_url).await?;
    log::info!(
        "CTDB repair: downloaded {} bytes of parity",
        parity_bytes.len()
    );

    // 2. Decode the full image once. Try ffmpeg first, fall back to wvunpack
    //    for WavPack v4 files that ffmpeg can't read.
    let _ = tx
        .send(AppMessage::StatusMessage(format!(
            "CTDB repair: decoding image ({} tracks)...",
            n
        )))
        .await;

    let audio_path = info.audio_path.clone();
    let raw_i16 = tokio::task::spawn_blocking(move || {
        super::accuraterip::decode_track_to_raw_i16(&audio_path)
            .or_else(|_| super::accuraterip::decode_to_raw_i16_wvunpack(&audio_path))
    })
    .await
    .map_err(|e| format!("Decode task failed: {}", e))?
    .map_err(|e| format!("Image decode error: {}", e))?;

    let audio_len = raw_i16.len();

    // 3. Assemble disc image: [STRIDE zeros] + [audio] + [STRIDE zeros].
    let stride = crate::ctdb_rs::STRIDE;
    let image_len = stride + audio_len + stride;
    let mut image: Vec<i16> = Vec::with_capacity(image_len);
    image.extend(std::iter::repeat(0i16).take(stride));
    image.extend_from_slice(&raw_i16);
    image.extend(std::iter::repeat(0i16).take(stride));
    drop(raw_i16); // freed; reconstructed from `image` after repair

    // 4. Run RS repair.
    let _ = tx
        .send(AppMessage::StatusMessage(
            "CTDB repair: running Reed-Solomon repair...".into(),
        ))
        .await;

    let repair_result = tokio::task::spawn_blocking(move || {
        repair_disc_via_rs_with_npar_blocking(&mut image, &parity_bytes, npar, offset)
            .map(|r| (r, image))
    })
    .await
    .map_err(|e| format!("Repair task failed: {}", e))?;

    let (result, repaired_image) = match repair_result {
        Ok((r, img)) => (r, img),
        Err(e) => return Err(format!("Repair failed: {}", e)),
    };

    log::info!(
        "CTDB repair: corrected {} samples at {} positions",
        result.corrected_samples,
        result.error_positions.len()
    );

    if result.corrected_samples == 0 {
        return Ok("No errors found — repair not needed".to_string());
    }

    // 5. Slice repaired audio out of the disc image (drop leadin/leadout).
    let repaired_audio: Vec<i16> = repaired_image[stride..stride + audio_len].to_vec();
    drop(repaired_image); // free memory

    // 6. Encode the repaired audio as a single file in /tmp.
    let pid = std::process::id();
    let tmp_dir = PathBuf::from(format!("/tmp/tonepoet-ctdb-repair-{}", pid));
    std::fs::create_dir_all(&tmp_dir).map_err(|e| format!("Failed to create temp dir: {}", e))?;

    let filename = info
        .audio_path
        .file_name()
        .ok_or_else(|| "Audio path has no filename".to_string())?;
    let tmp_out = tmp_dir.join(filename);

    let _ = tx
        .send(AppMessage::StatusMessage(
            "CTDB repair: re-encoding image...".into(),
        ))
        .await;

    super::accuraterip::encode_corrected_track(&repaired_audio, &tmp_out, &info.audio_path)
        .await
        .map_err(|e| {
            let _ = std::fs::remove_dir_all(&tmp_dir);
            format!("Image encode failed: {}", e)
        })?;

    super::accuraterip::copy_metadata(&info.audio_path, &tmp_out)
        .await
        .map_err(|e| {
            let _ = std::fs::remove_dir_all(&tmp_dir);
            format!("Metadata copy failed: {}", e)
        })?;

    // 7. Verify per-track CRC32 using CUE boundaries.
    let _ = tx
        .send(AppMessage::StatusMessage(
            "CTDB repair: verifying repaired tracks...".into(),
        ))
        .await;

    let suffix_skip = compute_suffix_skip(info.total_samples);

    for (i, &(start_sample, sample_count)) in info.track_boundaries.iter().enumerate() {
        let start = start_sample as usize * 2; // stereo-pair count → i16 count
        let count = sample_count as usize * 2;
        let end = (start + count).min(repaired_audio.len());
        if start >= repaired_audio.len() {
            let _ = std::fs::remove_dir_all(&tmp_dir);
            return Err(format!(
                "Track {} boundary {} starts beyond repaired audio (len {})",
                i + 1,
                start,
                repaired_audio.len(),
            ));
        }
        let track_audio = &repaired_audio[start..end];
        let is_first = i == 0;
        let is_last = i == n - 1;
        let computed = compute_track_crc32(track_audio, is_first, is_last, suffix_skip);
        let expected = expected_crcs[i];
        if computed != expected {
            let _ = std::fs::remove_dir_all(&tmp_dir);
            return Err(format!(
                "Track {} verification failed: repaired CRC {:08X} != expected {:08X}. \
                 Original not modified.",
                i + 1,
                computed,
                expected,
            ));
        }
        log::info!(
            "CTDB repair: track {} verified CRC32 = {:08X} ✓",
            i + 1,
            computed,
        );
    }

    // 8. Replace the original image with backup/restore.
    let _ = tx
        .send(AppMessage::StatusMessage(
            "CTDB repair: replacing original...".into(),
        ))
        .await;

    let orig = &info.audio_path;
    let orig_ext = orig.extension().and_then(|e| e.to_str()).unwrap_or("");
    let bak = orig.with_extension(format!("{}.bak", orig_ext));

    if let Err(e) = std::fs::copy(orig, &bak) {
        // Clean up any partial .bak that was written before the error.
        let _ = std::fs::remove_file(&bak);
        let _ = std::fs::remove_dir_all(&tmp_dir);
        return Err(format!("Backup failed for {}: {}", orig.display(), e));
    }

    if let Err(e) = std::fs::copy(&tmp_out, orig) {
        // Restore from backup. If the original was partially overwritten,
        // copying the .bak back fixes it.
        let _ = std::fs::copy(&bak, orig);
        let _ = std::fs::remove_file(&bak);
        let _ = std::fs::remove_dir_all(&tmp_dir);
        return Err(format!("Replace failed for {}: {}", orig.display(), e));
    }

    let _ = std::fs::remove_file(&bak);
    let _ = std::fs::remove_dir_all(&tmp_dir);

    Ok(format!(
        "CTDB repair: corrected {} samples in {} positions ({} tracks, single-image)",
        result.corrected_samples,
        result.error_positions.len(),
        n,
    ))
}
