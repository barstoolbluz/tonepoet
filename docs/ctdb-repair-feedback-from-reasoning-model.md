Key correction to the brief

The empirical discovery is directionally right but the algorithm phrasing needs one important correction.

The XML syndrome bytes are indeed the same one-row bytes that appear as row 0 of the downloadable full blob. CUETools.NET confirms this: FetchDB reads the full blob with Bytes2Syndrome(...) and explicitly checks that full row 0 equals entry.syndrome[0, i].

But CUETools does not compare that row against a raw “parity matrix row” from the local audio. It compares it against:

ar.GetSyndrome(npar, 1, -offset)

inside CDRepairEncode.FindOffset. That means the local audio row must be converted into CUETools’ one-row syndrome orientation before the XOR/BM/Chien test. The raw-row experiment from the prior model is therefore exactly the kind of thing that can produce spurious BM “successes” near a boundary.

So the corrected fast path is:

Decode entry row:
syndrome="...": base64 decode as little-endian u16, direct.
legacy inline parity="...": base64 decode, then convert with CUETools’ one-row Parity2Syndrome transform.
For each candidate offset O in stereo sample pairs:
take local source row (-2 * O) mod STRIDE;
convert that local row with the same one-row parity-to-syndrome transform;
XOR local row with entry row;
accept exact zero immediately;
otherwise run Berlekamp-Massey and validate with Chien search against stridecount.
Sort entries by descending confidence; first verified hit wins.
Only if that entry’s fast path does not verify and hasparity exists, try the existing exact full-parity fallback.

CUETools’ FindOffset itself uses BM + Chien; Forney appears later in VerifyParity, which is the full all-column recovery/repair path.

Answers to the six questions
1. Algorithm confirmation

CUETools’ call chain is:

CTDBResponseEntry exposes parity, syndrome, hasparity, npar, stride, trackcrcs, etc.

DBEntry normalizes the XML entry into a ushort[,] syndrome: if syndrome is absent it converts legacy inline parity with Parity2Syndrome(1, 1, 8, 8, ...); otherwise it loads syndrome directly with Bytes2Syndrome(1, min(maxNpar, npar), ...).

CUEToolsDB.DoVerify calls verify.FindOffset(entry.syndrome, entry.crc, out entry.offset, out entry.hasErrors) for each entry. If that reports errors and full parity is available, it fetches the blob and calls VerifyParity.

FindOffset searches offsets, computes ar.GetSyndrome(npar, 1, -offset), XORs row 0 with the entry row, accepts exact zero, otherwise runs BM and validates roots with chienSearch(..., stridecount, ...).

So: BM + Chien is the correct fast-path test, but the local row is not just raw audio.parity[row]; it is the one-row syndrome produced from that row.

2. Offset semantics

CUETools’ FindOffset loop is:

for (int offset = 1 - stride / 2; offset < stride / 2; offset++)

With CTDB stride 11760 i16 words, that is -5879..+5879 stereo sample pairs.

AccurateRipVerify.GetSyndrome documents that offset is constrained by Abs(offset * 2) < stride, and it passes -offset * 2 into Parity2Syndrome, so the units are stereo sample pairs and the matrix shift is in 16-bit words.

An AccurateRip/EAC drive offset is also in CD audio samples/stereo sample pairs, but the sign convention is easy to mix up. CUETools stores entry.offset, then uses -entry.offset when computing CTDB track CRCs. The EAC log’s Read offset correction: +6 means EAC applied that correction during extraction; it does not prove CTDB’s discovered actualOffset will be +6. The patch therefore searches the full CUETools window instead of trusting the log offset.

3. Fast-path scope

CUETools fast path verifies only one row: GetSyndrome(npar, 1, -offset). Full parity is fetched only when fast path found a related entry with errors and hasparity exists. Then VerifyParity iterates all stride columns and uses BM/Chien/Forney plus a CRC residual check.

Implication: a column-0-only VerifiedRs is a CUETools-compatible fast verification signal, but it is not the same as an all-column proof. The false failure/false acceptance rate is not a fixed constant; it depends on how differences are distributed modulo the CTDB stride. For a future stronger implementation, add a dry-run full correctability verifier to ctdb_rs or call repair on a clone when hasparity is present and column 0 reports nonzero correctable errors.

The patch preserves your requested constraint: it does not modify src/ctdb_rs/mod.rs; it uses the existing exact full-parity verifier only as a fallback when the one-row fast path is inconclusive.

4. npar=8 inline parity="..."

Inline parity is not byte-equivalent to syndrome. CUETools writes:

form.Add("parity", Convert.ToBase64String(
    ParityToSyndrome.Syndrome2Parity(verify.AR.GetSyndrome(8, 1, offset))
));
form.Add("syndrome", Convert.ToBase64String(
    ParityToSyndrome.Syndrome2Bytes(verify.AR.GetSyndrome(npar, 1, offset))
));

and reads them back by converting inline parity with Parity2Syndrome, while reading syndrome directly with Bytes2Syndrome.

So they are semantically equivalent after normalization to a one-row syndrome, but not the same wire format.

5. Implementation

No src/ctdb_rs/mod.rs change is required. The patch only targets src/tui/ctdb.rs.

Add/ensure the import and entry parsing pieces:

// in src/tui/ctdb.rs (imports)
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use base64::Engine as _;
// in src/tui/ctdb.rs (ensure CtdbEntry has these fields)
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
// in src/tui/ctdb.rs (inside parse_ctdb_response)
let has_parity = extract_attr(trimmed, "hasparity");
let parity = extract_attr(trimmed, "parity");
let syndrome = extract_attr(trimmed, "syndrome");

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

Replace the broken v2 syndrome verification block with this:

// in src/tui/ctdb.rs (replace verify_disc_via_rs / verify_disc_via_syndromes_blocking
// / syndrome_row_from_entry / parity_matrix_row_to_syndrome_row)

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RsVerifySource {
    /// CTDB XML `syndrome="..."`: bytes are already the one-row syndrome
    /// written by CUETools as Syndrome2Bytes(GetSyndrome(npar, 1, offset)).
    Syndrome,
    /// Legacy CTDB XML `parity="..."`: bytes are the one-row parity form
    /// written by CUETools as Syndrome2Parity(GetSyndrome(8, 1, offset)).
    InlineParity,
    /// Downloaded `hasparity` blob, checked with the existing codec verifier.
    FullParity,
}

#[derive(Debug, Clone)]
pub struct RsVerifiedMatch {
    pub entry: CtdbEntry,
    /// CUETools-style actualOffset, in stereo sample pairs.
    /// The matching row is (-2 * offset) modulo crate::ctdb_rs::STRIDE.
    pub offset: i32,
    pub confidence: u32,
    pub npar: u32,
    pub source: RsVerifySource,
    /// Number of column-0 RS errors found by BM/Chien. Exact full-parity
    /// verification leaves this as None.
    pub column0_errors: Option<usize>,
}

#[derive(Debug, Clone)]
struct EntryFastRow {
    words: Vec<u16>,
    source: RsVerifySource,
}

/// CUETools-compatible offset search window.
/// For CTDB STRIDE=11760 i16 words, this is -5879..+5879 stereo sample pairs.
fn ctdb_offset_window(_requested: i32) -> i32 {
    (crate::ctdb_rs::STRIDE / 2 - 1) as i32
}

fn ctdb_offset_range(window: i32) -> std::ops::RangeInclusive<i32> {
    (-window)..=window
}

fn row_for_sample_offset(offset: i32) -> usize {
    let stride = crate::ctdb_rs::STRIDE as i64;
    ((-2_i64 * offset as i64).rem_euclid(stride)) as usize
}

fn stridecount_for_image(audio_image: &[i16]) -> Option<usize> {
    let stride = crate::ctdb_rs::STRIDE;
    if audio_image.len() < stride * 2 || audio_image.len() % stride != 0 {
        return None;
    }
    Some(audio_image.len() / stride - 2)
}

fn decode_base64_u16_le_row(value: &str, npar: usize) -> Option<Vec<u16>> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(value.trim())
        .ok()?;

    if bytes.len() < npar * 2 {
        return None;
    }

    let mut out = Vec::with_capacity(npar);
    for i in 0..npar {
        out.push(u16::from_le_bytes([bytes[i * 2], bytes[i * 2 + 1]]));
    }
    Some(out)
}

/// CUETools' ParityToSyndrome.Parity2Syndrome for one row.
///
/// This is needed both for legacy inline `parity="..."` entries and for
/// converting a row from our computed CTDB parity matrix into the one-row
/// syndrome shape consumed by CUETools FindOffset.
fn parity_matrix_row_to_syndrome_row(
    gf: &crate::ctdb_rs::Galois16,
    parity_matrix: &[Vec<u16>],
    row: usize,
    npar: usize,
) -> Option<Vec<u16>> {
    let parity_row = parity_matrix.get(row)?;
    parity_row_to_syndrome_row(gf, parity_row, npar)
}

fn parity_row_to_syndrome_row(
    gf: &crate::ctdb_rs::Galois16,
    parity_row: &[u16],
    npar: usize,
) -> Option<Vec<u16>> {
    if parity_row.len() < npar {
        return None;
    }

    let mut syndrome = vec![0u16; npar];
    let gf_max = crate::ctdb_rs::galois::MAX;

    for x1 in 0..npar {
        let lo = parity_row[x1];
        if lo == 0 {
            continue;
        }

        let log_lo = gf.log(lo)?;
        for x in 0..npar {
            let decrement = ((1 + x1) * x) % gf_max;
            let exp = (log_lo + gf_max - decrement) % gf_max;
            syndrome[x] ^= gf.alpha_pow(exp);
        }
    }

    Some(syndrome)
}

fn syndrome_row_from_entry(
    gf: &crate::ctdb_rs::Galois16,
    entry: &CtdbEntry,
    npar: usize,
) -> Option<EntryFastRow> {
    if let Some(s) = entry.syndrome.as_deref().filter(|s| !s.trim().is_empty()) {
        let words = decode_base64_u16_le_row(s, npar)?;
        return Some(EntryFastRow { words, source: RsVerifySource::Syndrome });
    }

    if let Some(p) = entry.parity.as_deref().filter(|p| !p.trim().is_empty()) {
        let parity_row = decode_base64_u16_le_row(p, npar)?;
        let words = parity_row_to_syndrome_row(gf, &parity_row, npar)?;
        return Some(EntryFastRow { words, source: RsVerifySource::InlineParity });
    }

    None
}

fn xor_rows(lhs: &[u16], rhs: &[u16]) -> Vec<u16> {
    lhs.iter().zip(rhs.iter()).map(|(&a, &b)| a ^ b).collect()
}

fn correctable_error_count(
    gf: &crate::ctdb_rs::Galois16,
    delta: &[u16],
    npar: usize,
    stridecount: usize,
) -> Option<usize> {
    if delta.iter().all(|&w| w == 0) {
        return Some(0);
    }

    let max_correctable = npar / 2;
    let (sigma, errors_found) = crate::ctdb_rs::berlekamp_massey(gf, delta, npar)?;
    if errors_found == 0 || errors_found > max_correctable {
        return None;
    }

    let positions = crate::ctdb_rs::chien_search(gf, &sigma, errors_found, stridecount)?;
    if positions.len() == errors_found {
        Some(errors_found)
    } else {
        None
    }
}

fn compute_cached_parity_matrices(
    audio_image: &[i16],
    npars: &[usize],
) -> HashMap<usize, Vec<Vec<u16>>> {
    let gf = crate::ctdb_rs::Galois16::new();

    let mut unique = BTreeSet::new();
    for &npar in npars {
        if npar == 8 || npar == 16 {
            unique.insert(npar);
        }
    }

    let mut out = HashMap::new();
    for npar in unique {
        match crate::ctdb_rs::syndrome::compute_parity_matrix_from_audio(&gf, audio_image, npar) {
            Ok(matrix) => {
                out.insert(npar, matrix);
            }
            Err(e) => {
                log::warn!("CTDB RS: failed to compute parity matrix for npar={}: {}", npar, e);
            }
        }
    }

    out
}

fn verify_entry_via_syndrome_fast_path(
    gf: &crate::ctdb_rs::Galois16,
    parity_matrix: &[Vec<u16>],
    entry: &CtdbEntry,
    offset_window: i32,
    stridecount: usize,
) -> Option<RsVerifiedMatch> {
    let npar = (entry.npar as usize).min(16);
    if npar == 0 || npar > 16 {
        return None;
    }

    let entry_row = syndrome_row_from_entry(gf, entry, npar)?;
    if entry_row.words.len() != npar {
        return None;
    }

    let mut best: Option<(i32, usize)> = None;

    for offset in ctdb_offset_range(offset_window) {
        let row = row_for_sample_offset(offset);
        let computed_row = match parity_matrix_row_to_syndrome_row(gf, parity_matrix, row, npar) {
            Some(row) => row,
            None => continue,
        };

        let delta = xor_rows(&computed_row, &entry_row.words);

        match correctable_error_count(gf, &delta, npar, stridecount) {
            Some(0) => {
                return Some(RsVerifiedMatch {
                    entry: entry.clone(),
                    offset,
                    confidence: entry.confidence,
                    npar: npar as u32,
                    source: entry_row.source,
                    column0_errors: Some(0),
                });
            }
            Some(errors) => {
                let replace = match best {
                    None => true,
                    Some((best_offset, best_errors)) => {
                        errors < best_errors
                            || (errors == best_errors && offset.abs() < best_offset.abs())
                    }
                };

                if replace {
                    best = Some((offset, errors));
                }
            }
            None => {}
        }
    }

    best.map(|(offset, errors)| RsVerifiedMatch {
        entry: entry.clone(),
        offset,
        confidence: entry.confidence,
        npar: npar as u32,
        source: entry_row.source,
        column0_errors: Some(errors),
    })
}

fn verify_entry_full_parity_exact_blocking(
    audio_image: &[i16],
    parity_bytes: &[u8],
    entry: &CtdbEntry,
    offset_window: i32,
) -> Option<RsVerifiedMatch> {
    let npar = (entry.npar as usize).min(16);
    if npar == 0 || npar > 16 {
        return None;
    }

    let codec = crate::ctdb_rs::CtdbCodec::new();

    for offset in ctdb_offset_range(offset_window) {
        let word_offset = i64::from(offset) * 2;

        match codec.try_verify_with_word_offset(audio_image, parity_bytes, npar, word_offset) {
            Ok(result) if result.matches => {
                return Some(RsVerifiedMatch {
                    entry: entry.clone(),
                    offset,
                    confidence: entry.confidence,
                    npar: npar as u32,
                    source: RsVerifySource::FullParity,
                    column0_errors: None,
                });
            }
            Ok(_) => {}
            Err(e) => {
                log::debug!(
                    "CTDB RS: full parity verify failed for entry {} at offset {:+}: {}",
                    entry.id,
                    offset,
                    e
                );
            }
        }
    }

    None
}

/// Verify a CTDB disc image via CUETools-compatible one-row syndrome data,
/// falling back to exact full-parity verification only when an entry exposes a
/// `hasparity` URL and the one-row fast path does not verify that entry.
pub async fn verify_disc_via_rs(
    audio_image: &[i16],
    entries: &[CtdbEntry],
    offset_window: i32,
) -> Option<RsVerifiedMatch> {
    if audio_image.is_empty() || entries.is_empty() {
        return None;
    }

    let stridecount = match stridecount_for_image(audio_image) {
        Some(rows) => rows,
        None => {
            log::warn!(
                "CTDB RS: audio image length {} is not a valid CTDB image length",
                audio_image.len()
            );
            return None;
        }
    };

    let offset_window = ctdb_offset_window(offset_window);

    let mut ordered = entries.to_vec();
    ordered.sort_by(|a, b| {
        b.confidence
            .cmp(&a.confidence)
            .then_with(|| a.id.cmp(&b.id))
    });

    let npars: Vec<usize> = ordered
        .iter()
        .filter(|e| {
            e.syndrome.as_deref().map_or(false, |s| !s.trim().is_empty())
                || e.parity.as_deref().map_or(false, |p| !p.trim().is_empty())
        })
        .map(|e| (e.npar as usize).min(16))
        .collect();

    let audio_for_parity = audio_image.to_vec();
    let parity_matrices = tokio::task::spawn_blocking(move || {
        compute_cached_parity_matrices(&audio_for_parity, &npars)
    })
    .await
    .ok()?;

    let gf = crate::ctdb_rs::Galois16::new();
    let mut parity_download_cache: HashMap<String, Vec<u8>> = HashMap::new();

    for entry in ordered {
        let npar = (entry.npar as usize).min(16);

        if let Some(matrix) = parity_matrices.get(&npar) {
            if let Some(hit) = verify_entry_via_syndrome_fast_path(
                &gf,
                matrix,
                &entry,
                offset_window,
                stridecount,
            ) {
                log::info!(
                    "CTDB RS: verified entry {} confidence {} via {:?} at offset {:+}, column0_errors={:?}",
                    hit.entry.id,
                    hit.confidence,
                    hit.source,
                    hit.offset,
                    hit.column0_errors
                );
                return Some(hit);
            }
        }

        let url = match entry.has_parity.as_deref().filter(|s| !s.trim().is_empty()) {
            Some(url) => url.to_string(),
            None => continue,
        };

        let parity_bytes = if let Some(bytes) = parity_download_cache.get(&url) {
            bytes.clone()
        } else {
            match download_parity(&url).await {
                Ok(bytes) => {
                    parity_download_cache.insert(url.clone(), bytes.clone());
                    bytes
                }
                Err(e) => {
                    log::warn!("CTDB RS: failed to download parity for entry {}: {}", entry.id, e);
                    continue;
                }
            }
        };

        let audio_for_full = audio_image.to_vec();
        let entry_for_full = entry.clone();

        if let Some(hit) = tokio::task::spawn_blocking(move || {
            verify_entry_full_parity_exact_blocking(
                &audio_for_full,
                &parity_bytes,
                &entry_for_full,
                offset_window,
            )
        })
        .await
        .ok()
        .flatten()
        {
            log::info!(
                "CTDB RS: verified entry {} confidence {} via full parity at offset {:+}",
                hit.entry.id,
                hit.confidence,
                hit.offset
            );
            return Some(hit);
        }
    }

    None
}
6. Failure-mode tests

Test these specifically:

Known Allman Brothers SHM case: should verify against the confidence-896 entry.
Exact common pressing: column0_errors=Some(0) and per-track CRCs should be Verified.
Offset boundary: known shifted data at -5879, 0, +5879 should pass; ±5880 should not be searched.
Legacy inline parity only: an npar=8 entry without syndrome should normalize through Parity2Syndrome.
Random wrong-disc entry: BM alone may return a polynomial; Chien validation must reject it.
Column-0 pass but full-column fail: should be flagged as the known limitation of column-0 fast verification unless a future full correctable verifier is added.
Multi-disc directory: ensure each disc’s TOC is queried independently and entries from disc N cannot verify disc M.
Sign convention regression: compare the discovered offset against CUETools on at least one shifted fixture; if UI displays offset to users, label the sign convention explicitly.
