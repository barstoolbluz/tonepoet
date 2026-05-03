# CTDB Syndrome Verification — Self-Contained Bundle

This single document contains everything a reasoning model needs to diagnose the issue and propose+generate code:

1. **Brief** — problem statement, empirical evidence, asks (Sections A–D).
2. **Source: `src/tui/ctdb.rs`** — full file. The verification call site we're modifying.
3. **Source: `src/ctdb_rs/mod.rs` (API surface)** — Reed-Solomon codec we'll be calling. Internals omitted (~700 lines of GF arithmetic, Berlekamp-Massey, Chien search, Forney algorithm — the public API in this excerpt is sufficient).
4. **Source: `src/tui/cue_parser.rs` (excerpt)** — `SingleImageInfo` struct.
5. **Source: `src/tui/app.rs` (excerpt)** — `CtdbVerifyState` / `CtdbVerifyPage` overlay state.
6. **Source: `src/tui/command.rs` (excerpt)** — `Command::Ctdb` and `Command::CtdbRepair` dispatch.

---

# 1. Brief

## TL;DR

Our `:ctdb` (CUETools Database) verification reports per-track CRC32 mismatches against the highest-confidence database entry, while CUETools on Windows reports the same rip as `verified OK, confidence 896`. We have empirical evidence that our CRC32 logic itself is correct; the gap is that CTDB's strongest verification signal is a Reed-Solomon **syndrome** check (or full RS verification using downloadable parity bytes), not the per-track CRC32 lookup we're doing. **We need to add RS-syndrome-based verification.** Our codebase already contains a complete Galois-16 RS codec with `try_verify_with_word_offset(audio, parity_bytes, npar, word_offset) -> VerifyResult`. We need a reasoning model to (a) confirm the diagnosis, (b) design the verification flow, (c) generate the wiring code.

## What CTDB is

CUETools Database (CTDB), `https://db.cue.tools/lookup2.php`, is a community CD verification database. A submission consists of a TOC (track sector offsets), per-track CRC32 values, a Reed-Solomon parity matrix over the disc's audio, and a compact "syndrome" derived from that parity. The disc image is conceptually the concatenation `[STRIDE leadin zeros] + [track1 audio] + [track2 audio] + … + [STRIDE leadout zeros]` where `STRIDE = 11_760` i16 values (= 5_880 stereo sample pairs = 10 CD sectors).

A CTDB lookup by TOC returns one or more `<entry>` elements; multiple entries can exist for the same TOC because pressings differ subtly (sometimes only in trailing samples or pre-emphasis flag). Each entry carries:

- `confidence="N"` — count of identical submissions.
- `trackcrcs="hex hex hex …"` — per-track CRC32 over each track's audio with a CTDB-specific prefix-skip on the first track and a TOC-derived suffix-skip on the last track.
- `npar="8"` or `npar="16"` — number of parity symbols per RS column (max correctable errors = `npar/2`).
- `stride="5880"` — CTDB stride in stereo pairs (matches our `STRIDE / 2`).
- Plus, for parity/syndrome data, **one of three patterns** observed empirically in the response below:
  1. `npar=8` with inline `parity="<base64>"`. No `syndrome` attribute, no `hasparity` URL.
  2. `npar=16` with `syndrome="<base64>"` and **no** parity. The syndrome is the only verification signal — full parity is not downloadable for these entries.
  3. `npar=16` (or `npar=8` for the high-confidence canonical) with both `syndrome="<base64>"` and `hasparity="<URL>"`. The URL serves the full parity matrix bytes.

The `syndrome` attribute, when present, decodes to `npar * 2` bytes — small (16 or 32 bytes) compared to a full parity matrix (`stride * npar * 2` = 94_080 bytes for npar=8, 188_160 bytes for npar=16). The exact algorithm CUETools uses to derive the syndrome from parity is what we'd need to replicate to do a syndrome-only fast verify; alternatively we can just download parity for entries that expose `hasparity`.

CUETools' "verified OK" means RS verification against the entry's syndrome/parity passes with **zero** RS errors at the rip's drive-read offset (CUETools also tries a small offset window). Per-track CRC32 mismatch is non-fatal — RS verification is the authoritative signal.

## What our implementation currently does (incorrect)

`src/tui/ctdb.rs::verify_ctdb` builds a TOC, queries CTDB, picks the entry with the highest confidence (`max_by_key(|e| e.confidence)`), then for each track:

```rust
let computed = compute_track_crc32(&decoded, is_first, is_last, suffix_skip);
let db_crc = entry.track_crcs.get(i).copied();
let status = match db_crc {
    Some(expected) if expected == computed => CtdbTrackStatus::Verified,
    Some(_) => CtdbTrackStatus::Mismatch,
    None => CtdbTrackStatus::Error("track not in CTDB entry".to_string()),
};
```

`compute_track_crc32` applies:
- `PREFIX_SKIP_I16 = STRIDE_WORDS = 11_760` (i16 count) at the start of the first track.
- `compute_suffix_skip(total_disc_samples) = STRIDE_WORDS + (total_disc_words % STRIDE_WORDS)` (i16 count) at the end of the last track.
- No skip on middle tracks.
- CRC32 (IEEE 802.3, via `crc32fast::hash`) over the i16 buffer reinterpreted as little-endian u8 bytes.

This logic is correct (validated below), but it only matches when the rip exactly reproduces the byte stream of whichever CTDB entry happens to have the highest confidence. For widely-pressed albums whose canonical 896-confidence entry came from a master tape that differs in trailing samples or pre-emphasis from typical commercial pressings, our rip ends up with CRCs that match a constellation of low-confidence entries (one per submitter pattern) rather than the high-confidence canonical, and we declare "Mismatch" — even though the rip is RS-verifiable against the canonical.

## Empirical evidence

Test disc: *The Allman Brothers Band – At Fillmore East Deluxe Edition (Disc 2) (Japan SHM)*. Four per-track FLACs, ripped by EAC with `Read offset correction: 6` and `Fill up missing offset samples with silence: Yes`. The EAC log's TOC matches the FLAC sample counts exactly, and the FLAC sample counts equal `disc sectors × 588`.

CUETools (Windows) on these exact files: `CTDB: verified OK, confidence 896, or differs in 125 samples, confidence 3, or differs in 44 samples, confidence 2`.

Our `:ctdb` against the same files: all 4 tracks "Mismatch".

A standalone Rust diagnostic decoded each FLAC via `ffmpeg -f s16le -ar 44100 -ac 2`, applied our exact `compute_track_crc32` logic, and compared against:

- the EAC log's per-track CRC32 (no skip — over the full WAV);
- the canonical (confidence 896) CTDB entry's `trackcrcs`.

| | EAC log CRC (no skip) | Our CTDB compute (with skip) | 896 entry `trackcrcs` |
|--|--|--|--|
| Track 1 | `00666F5F` | `c17a4a77` | `40c5dc10` |
| Track 2 | `FABB2BE6` | `fabb2be6` | `65dfcc8a` |
| Track 3 | `52898FE0` | `52898fe0` | `1ef7b539` |
| Track 4 | `D8316AE9` | `96af30c8` | `d21a8789` |

Tracks 2 and 3 (no skip in CTDB) — our CRC matches EAC's whole-track CRC32 exactly. Track 1 and Track 4's CRCs differ from EAC's because of CTDB's prefix/suffix skips, as designed. **Our CRC computation is correct.**

The 896 entry's `trackcrcs` simply describes a *different* byte sequence from this rip. Our rip's individual track CRCs do appear in CTDB — `c17a4a77` is the track-1 CRC of ~15 distinct confidence-1 entries, `fabb2be6` is track-2 of ~14 entries — but our specific 4-tuple `(c17a4a77, fabb2be6, 52898fe0, 96af30c8)` is not a single entry in the database. Our rip's audio is not byte-identical to any submitter's, but is RS-equivalent to the canonical (confidence 896) submission.

## The CTDB response (representative subset)

Sent: `GET https://db.cue.tools/lookup2.php?version=3&ctdb=1&toc=0:24159:127166:278762:309864`

```xml
<ctdb xmlns="http://db.cuetools.net/ns/mmd-1.0#" xmlns:ext="http://db.cuetools.net/ns/ext-1.0#">
 <entry confidence="896" crc32="13f525de" hasparity="http://p.cuetools.net/113068" id="113068"
        npar="16" stride="5880" syndrome="HI0I8cxLz56i0JeEyXcrAKXVs/JFVwngnkaZ8a3jO58="
        toc="0:24159:127166:278762:309864" trackcrcs="40c5dc10 65dfcc8a 1ef7b539 d21a8789" />
 <entry confidence="3" crc32="5701aff0" hasparity="http://p.cuetools.net/8888389" id="8888389"
        npar="8" stride="5880" syndrome="t8QZhJ3lAyZZZkSjASM7xA=="
        toc="0:24159:127166:278762:309864" trackcrcs="ca4cb88d 3fa34037 3df63007 8f4bfab1" />
 <entry confidence="2" crc32="30ff8c29" hasparity="http://p.cuetools.net/10527693" id="10527693"
        npar="8" stride="5880" syndrome="fXJG3GrPHlTfpvHQDeUi/g=="
        toc="0:24159:127166:278762:309864" trackcrcs="4b4fce69 4dd48851 affbbca3 310638ad" />
 <entry confidence="1" crc32="81ae880d" id="1867848" npar="8" parity="zqlFbO8Nfi1SnryIl2O2rw=="
        stride="5880" toc="0:24159:127166:278762:309864" trackcrcs="bb450280 c458b0b1 68919243 7569f562" />
 <entry confidence="1" crc32="27397da5" id="2374177" npar="8" parity="+ykHWZ3Z27G3FJm6gR4+aQ=="
        stride="5880" toc="0:24159:127166:278762:309864" trackcrcs="c17a4a77 fabb2be6 694a392b 2848faf3" />
 <!-- … 30+ more confidence=1 entries with `syndrome=…` (no full parity inline);
      see https://db.cue.tools/lookup2.php?version=3&ctdb=1&toc=0:24159:127166:278762:309864 -->
 <metadata />
</ctdb>
```

Notes (verified against the actual response):
- **`npar=8` entries with inline `parity="<base64>"`** carry no `syndrome` attribute. The inline parity is small enough to embed (npar=8 inline `parity` empirically decodes to 16 bytes — that's actually one column's parity, not the full matrix; check this against CUETools.NET to confirm whether it's a header-only parity or whether CTDB serves an abbreviated matrix here).
- **`hasparity="<URL>"`** points to a downloadable blob containing parity bytes laid out as our `bytes_to_parity` expects (`stride * npar * 2` bytes). Both `npar=8` and `npar=16` entries can have this.
- **`syndrome="<base64>"`** appears on `npar=16` entries (and `npar=8` entries that also have `hasparity`). It decodes to `npar * 2` bytes (16 or 32) — much smaller than the full parity matrix. The exact algorithm CUETools uses to derive the syndrome from the parity matrix is what we would need to replicate to do a syndrome-only verify without downloading parity; alternatively, downloading parity from `hasparity` lets us call our existing `try_verify_with_word_offset` directly.
- The 896-confidence canonical entry in this disc's response has `npar=16`, `syndrome="HI0I8cxLz56i0JeEyXcrAKXVs/JFVwngnkaZ8a3jO58="` (32 decoded bytes), AND `hasparity="http://p.cuetools.net/113068"` — both verification paths are available for it.

## What we want from a reasoning model

Please produce a single response that addresses **all** of the following:

### A. Diagnosis confirmation

Given the empirical CRC table above, the CTDB response sample, and our RS-codec primitives, confirm or correct this hypothesis:

> CUETools' `verified OK, confidence 896` is the result of an RS verification against the 896 entry's parity (or syndrome), not a per-track CRC32 match. Our rip's audio is RS-equivalent to (and therefore a valid alternate-pressing of) the 896-canonical, but not byte-identical, so its per-track CRC32s do not match the 896 entry's `trackcrcs`. To match CUETools' semantics, we must verify against entries' parity/syndrome, not just `trackcrcs`.

If this hypothesis is wrong or incomplete, explain why.

### B. Verification design

Specify the algorithm we should implement. At minimum address:

1. **What disc-image buffer do we hand the codec?** Per-track flow: `[STRIDE zeros] + concat(decoded tracks in TOC order) + [STRIDE zeros]`. Single-image flow: same with one decoded image. Confirm or correct.
2. **Which entries do we verify against and in what order?** Specifically: should we (a) iterate all entries highest-confidence-first and pick the first that verifies, or (b) verify against all and report the highest-confidence match? What about offset trial — do we attempt a small `±N` window of stereo-sample offsets per entry, or only offset 0? CUETools tries small offsets; what's the standard window?
3. **Parity vs syndrome.** Some entries embed a small `parity="<base64>"` (NPAR=8) directly; most provide only `hasparity="<URL>"` (NPAR=16). All entries provide a base64 `syndrome` field. Should we always download parity and use `try_verify_with_word_offset`, or can we replicate CUETools' "syndrome-only" check from the `syndrome` field alone? If syndrome-only is feasible, what's the algorithm to derive a verification yes/no from the entry's `syndrome` string and our rip's audio without downloading parity? (Hint: CUETools.NET's `CTDBResponseEntry.HasParity` and `CDRepairEncode` are likely sources; cite or reproduce the relevant logic if you can.)
4. **Result semantics.** When an entry verifies via RS but our per-track CRC32 against that entry's `trackcrcs` still doesn't match (because the byte sequences differ but parity tolerates it), what status do we report? Proposed: `CtdbTrackStatus::VerifiedRs` (RS-verified, CRC differs) distinct from `Verified` (CRC matches), distinct from `Mismatch` (RS verification failed). UI consumers of `:ctdb-repair` should treat `VerifiedRs` as "no repair needed" — not the same as a CRC32 mismatch.
5. **Backward compatibility.** Repair flow currently uses the matched entry's `trackcrcs` for post-repair CRC32 verification. If we adopt RS-syndrome verification for the verify path, what does the post-repair check do? (Likely: still compute per-track CRC32 from the *repaired* audio, but compare against the canonical entry's `trackcrcs` — repair is supposed to bring the audio to byte-identical-with-canonical, so post-repair CRC32 *should* now match.)
6. **Multi-entry trial.** If RS verification at offset 0 fails for the 896 entry but succeeds for a confidence-3 entry, what do we report? Probably the 896-fail and the 3-confirm should both surface — UI proposal: show the highest-confidence verified entry, with a note if it's not the highest-confidence in the response.
7. **Performance.** RS verification involves multiplying `STRIDE × NPAR` GF(2^16) matrix syndromes. The codec is single-threaded; for a 70-min disc the audio buffer is ~700 MB i16. Is per-entry RS verify fast enough to iterate dozens of entries, or do we need to verify against just the top-K? Estimate.

### C. Implementation

Generate Rust code that adds RS-syndrome verification to `src/tui/ctdb.rs`, with these constraints:

1. Keep the existing `verify_ctdb` and `verify_ctdb_single_image` async functions' return type `CtdbVerifyResult` — extend if needed (new variant on `CtdbTrackStatus`, new fields on `CtdbVerifyResult`), but don't break existing call sites in `src/tui/command.rs` more than necessary.
2. Add `syndrome: Option<String>` and `parity: Option<String>` (base64) to `CtdbEntry`; extend `parse_ctdb_response` to extract them.
3. Implement `verify_disc_via_rs(audio_image: &[i16], entries: &[CtdbEntry], offset_window: i32) -> Option<RsVerifiedMatch>` (or whatever signature you find natural) that iterates entries, downloads parity for each as needed (cache by URL), runs `CtdbCodec::try_verify_with_word_offset` at each offset in the window, returns the highest-confidence verified entry.
4. Wire it into the existing track-CRC32 path: per-track CRC32 still runs and is shown in the overlay (useful diagnostic), but the OVERALL "verified" status comes from RS verification.
5. If you propose a `syndrome`-only fast path that avoids the parity download, implement it; otherwise note that CUETools' syndrome encoding is opaque and we must download parity.
6. Keep the existing `download_parity(url)` helper; cache results in an `HashMap<String, Vec<u8>>` for the duration of one verify call.
7. The codec's verify entry points are synchronous and CPU-bound — wrap in `tokio::task::spawn_blocking`.
8. Don't change the repair flow other than: post-repair, CRC32 verification *should* now succeed against the canonical entry's `trackcrcs` (since RS repair brings audio to canonical bytes); if it doesn't, surface the discrepancy (don't replace originals).

Output Rust source in fenced code blocks with clear `// in src/tui/ctdb.rs` annotations identifying the target file/region for each block. We do not need a complete file rewrite — diffs/insertions are fine, as long as the integration points are unambiguous.

### D. Open questions to flag back

If during your design you encounter ambiguity that requires source documentation we should fetch (CUETools.NET source, CTDB schema docs, RS codec spec details), list those questions in a final section so we can resolve them before writing more code.

## Reference materials you may want

- CUETools.NET source: https://github.com/gchudov/cuetools.net (in particular `CUETools.CTDB/CDRepairEncode.cs`, `CTDBResponseEntry.cs`, `CDRepairEncodeData.cs`).
- CTDB API (no public schema doc; sample request/response above).
- Reed-Solomon over GF(2^16) with primitive polynomial `0x1100B` (from CUETools.NET's `Galois.cs`).
- Our local RS codec: `src/ctdb_rs/mod.rs` (already-tested; 5 unit tests pass).
- Our current verify call site: `src/tui/ctdb.rs::verify_ctdb`, `verify_ctdb_single_image`.

---

# 2. Source: `src/tui/ctdb.rs` (full)

```rust
//! CUETools Database (CTDB) client: TOC lookup, CRC32 verification.
//!
//! CTDB uses standard CRC32 over raw audio PCM bytes (no sample skipping).
//! The API returns per-track CRC32 values and parity data availability
//! for future Reed-Solomon error repair.

use std::path::{Path, PathBuf};
use tokio::sync::mpsc;
use super::message::AppMessage;

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
    /// Expected CRC32 from the CTDB entry (for repair verification).
    pub expected_crc32: Option<u32>,
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
    /// Parity symbol count from the best CTDB entry (for repair).
    pub npar: Option<u32>,
    /// Stride from the best CTDB entry (for repair).
    pub stride: Option<usize>,
    /// Parity download URL from the best CTDB entry (for repair).
    pub parity_url: Option<String>,
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
                    expected_crc32: None,
                    has_parity: false,
                }).collect(),
                toc,
                npar: None, stride: None, parity_url: None,
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
                    expected_crc32: None,
                    has_parity: false,
                }).collect(),
                toc,
                npar: None, stride: None, parity_url: None,
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
                    expected_crc32: None,
                    has_parity: false,
                }).collect(),
                toc,
                npar: None, stride: None, parity_url: None,
            };
        }
    };

    // Save repair-relevant metadata from the entry.
    let result_npar = Some(entry.npar);
    let result_stride = Some(entry.stride);
    let result_parity_url = entry.has_parity.clone();

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
                    expected_crc32: None,
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
                    expected_crc32: None,
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
            expected_crc32: db_crc,
            has_parity: entry.has_parity.is_some(),
        });
    }

    CtdbVerifyResult {
        tracks: results, toc,
        npar: result_npar, stride: result_stride, parity_url: result_parity_url,
    }
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
                    expected_crc32: None,
                    has_parity: false,
                }).collect(),
                toc,
                npar: None, stride: None, parity_url: None,
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
                    expected_crc32: None,
                    has_parity: false,
                }).collect(),
                toc,
                npar: None, stride: None, parity_url: None,
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
                    expected_crc32: None,
                    has_parity: false,
                }).collect(),
                toc,
                npar: None, stride: None, parity_url: None,
            };
        }
    };

    let result_npar = Some(entry.npar);
    let result_stride = Some(entry.stride);
    let result_parity_url = entry.has_parity.clone();

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
                    expected_crc32: None,
                    has_parity: false,
                }).collect(),
                toc,
                npar: None, stride: None, parity_url: None,
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
                    expected_crc32: None,
                    has_parity: false,
                }).collect(),
                toc,
                npar: None, stride: None, parity_url: None,
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
                expected_crc32: None,
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
            expected_crc32: db_crc,
            has_parity: entry.has_parity.is_some(),
        });
    }

    CtdbVerifyResult {
        tracks: results, toc,
        npar: result_npar, stride: result_stride, parity_url: result_parity_url,
    }
}

// ── Parity download ─────────────────────────────────────────────────

/// Download parity data from the CTDB `hasparity` URL.
async fn download_parity(url: &str) -> Result<Vec<u8>, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("HTTP client error: {}", e))?;

    let resp = client.get(url)
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
    let _ = tx.send(AppMessage::StatusMessage("CTDB repair: downloading parity...".into())).await;
    let parity_bytes = download_parity(parity_url).await?;
    log::info!("CTDB repair: downloaded {} bytes of parity", parity_bytes.len());

    // 2. Decode all tracks to raw i16 (parallel decode, sequential concat).
    let _ = tx.send(AppMessage::StatusMessage(
        format!("CTDB repair: decoding {} tracks...", n),
    )).await;

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
        let data = handle.await
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
    let _ = tx.send(AppMessage::StatusMessage("CTDB repair: running Reed-Solomon repair...".into())).await;

    let parity_clone = parity_bytes;
    let repair_result = tokio::task::spawn_blocking(move || {
        let codec = crate::ctdb_rs::CtdbCodec::new();
        codec.repair(&mut image, &parity_clone, npar, offset)
            .map(|r| (r, image))
    }).await
        .map_err(|e| format!("Repair task failed: {}", e))?;

    let (result, repaired_image) = match repair_result {
        Ok((r, img)) => (r, img),
        Err(e) => return Err(format!("Repair failed: {}", e)),
    };

    log::info!("CTDB repair: corrected {} samples at {} positions",
        result.corrected_samples, result.error_positions.len());

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
    std::fs::create_dir_all(&tmp_dir)
        .map_err(|e| format!("Failed to create temp dir: {}", e))?;

    let _ = tx.send(AppMessage::StatusMessage(
        format!("CTDB repair: re-encoding {} tracks...", n),
    )).await;

    for (i, (track_data, original_path)) in repaired_tracks.iter().zip(paths.iter()).enumerate() {
        let out_path = tmp_dir.join(
            original_path.file_name().unwrap_or_default(),
        );

        super::accuraterip::encode_corrected_track(track_data, &out_path, original_path).await
            .map_err(|e| {
                let _ = std::fs::remove_dir_all(&tmp_dir);
                format!("Track {} encode failed: {}", i + 1, e)
            })?;

        super::accuraterip::copy_metadata(original_path, &out_path).await
            .map_err(|e| {
                let _ = std::fs::remove_dir_all(&tmp_dir);
                format!("Track {} metadata copy failed: {}", i + 1, e)
            })?;
    }

    // 7. Verify repaired files via CTDB CRC32.
    let _ = tx.send(AppMessage::StatusMessage("CTDB repair: verifying repaired files...".into())).await;

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
                    i + 1, computed, expected,
                ));
            }
            log::info!(
                "CTDB repair: track {} verified CRC32 = {:08X} ✓",
                i + 1, computed,
            );
        } else {
            log::warn!(
                "CTDB repair: track {} has no expected CRC, skipping verification (CRC = {:08X})",
                i + 1, computed,
            );
        }
    }

    // 8. Replace originals with backup/restore pattern.
    let _ = tx.send(AppMessage::StatusMessage("CTDB repair: replacing originals...".into())).await;

    // Phase 1: backup originals to .bak.
    let mut backed_up: Vec<PathBuf> = Vec::with_capacity(n);
    for path in paths {
        let orig_ext = path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
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
        result.corrected_samples, result.error_positions.len(), n,
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
            expected_crcs.len(), n,
        ));
    }

    // 1. Download parity.
    let _ = tx.send(AppMessage::StatusMessage(
        "CTDB repair: downloading parity...".into(),
    )).await;
    let parity_bytes = download_parity(parity_url).await?;
    log::info!("CTDB repair: downloaded {} bytes of parity", parity_bytes.len());

    // 2. Decode the full image once. Try ffmpeg first, fall back to wvunpack
    //    for WavPack v4 files that ffmpeg can't read.
    let _ = tx.send(AppMessage::StatusMessage(
        format!("CTDB repair: decoding image ({} tracks)...", n),
    )).await;

    let audio_path = info.audio_path.clone();
    let raw_i16 = tokio::task::spawn_blocking(move || {
        super::accuraterip::decode_track_to_raw_i16(&audio_path)
            .or_else(|_| super::accuraterip::decode_to_raw_i16_wvunpack(&audio_path))
    }).await
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
    let _ = tx.send(AppMessage::StatusMessage(
        "CTDB repair: running Reed-Solomon repair...".into(),
    )).await;

    let repair_result = tokio::task::spawn_blocking(move || {
        let codec = crate::ctdb_rs::CtdbCodec::new();
        codec.repair(&mut image, &parity_bytes, npar, offset)
            .map(|r| (r, image))
    }).await
        .map_err(|e| format!("Repair task failed: {}", e))?;

    let (result, repaired_image) = match repair_result {
        Ok((r, img)) => (r, img),
        Err(e) => return Err(format!("Repair failed: {}", e)),
    };

    log::info!("CTDB repair: corrected {} samples at {} positions",
        result.corrected_samples, result.error_positions.len());

    if result.corrected_samples == 0 {
        return Ok("No errors found — repair not needed".to_string());
    }

    // 5. Slice repaired audio out of the disc image (drop leadin/leadout).
    let repaired_audio: Vec<i16> = repaired_image[stride..stride + audio_len].to_vec();
    drop(repaired_image); // free memory

    // 6. Encode the repaired audio as a single file in /tmp.
    let pid = std::process::id();
    let tmp_dir = PathBuf::from(format!("/tmp/tonepoet-ctdb-repair-{}", pid));
    std::fs::create_dir_all(&tmp_dir)
        .map_err(|e| format!("Failed to create temp dir: {}", e))?;

    let filename = info.audio_path.file_name()
        .ok_or_else(|| "Audio path has no filename".to_string())?;
    let tmp_out = tmp_dir.join(filename);

    let _ = tx.send(AppMessage::StatusMessage(
        "CTDB repair: re-encoding image...".into(),
    )).await;

    super::accuraterip::encode_corrected_track(&repaired_audio, &tmp_out, &info.audio_path).await
        .map_err(|e| {
            let _ = std::fs::remove_dir_all(&tmp_dir);
            format!("Image encode failed: {}", e)
        })?;

    super::accuraterip::copy_metadata(&info.audio_path, &tmp_out).await
        .map_err(|e| {
            let _ = std::fs::remove_dir_all(&tmp_dir);
            format!("Metadata copy failed: {}", e)
        })?;

    // 7. Verify per-track CRC32 using CUE boundaries.
    let _ = tx.send(AppMessage::StatusMessage(
        "CTDB repair: verifying repaired tracks...".into(),
    )).await;

    let suffix_skip = compute_suffix_skip(info.total_samples);

    for (i, &(start_sample, sample_count)) in info.track_boundaries.iter().enumerate() {
        let start = start_sample as usize * 2; // stereo-pair count → i16 count
        let count = sample_count as usize * 2;
        let end = (start + count).min(repaired_audio.len());
        if start >= repaired_audio.len() {
            let _ = std::fs::remove_dir_all(&tmp_dir);
            return Err(format!(
                "Track {} boundary {} starts beyond repaired audio (len {})",
                i + 1, start, repaired_audio.len(),
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
                i + 1, computed, expected,
            ));
        }
        log::info!(
            "CTDB repair: track {} verified CRC32 = {:08X} ✓",
            i + 1, computed,
        );
    }

    // 8. Replace the original image with backup/restore.
    let _ = tx.send(AppMessage::StatusMessage(
        "CTDB repair: replacing original...".into(),
    )).await;

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
        result.corrected_samples, result.error_positions.len(), n,
    ))
}
```

---

# 3. Source: `src/ctdb_rs/mod.rs` (API surface only)

The full file is ~1100 lines. The internals (Galois-field tables, generator-polynomial construction, Berlekamp-Massey, Chien search, Forney algorithm, repair encoder) are CUETools-compatible and well-tested (5 unit tests pass) — we do **not** need to modify them. Only the public API is shown.

```rust
// CTDB Reed-Solomon codec over GF(2^16).
//
// GF polynomial: 0x1100B
// Generator roots: alpha^0 .. alpha^(npar-1)
// Data row `r` corresponds to locator exponent `data_rows - 1 - r`
// High-level offset parameters are CD stereo sample-pair offsets;
// helpers with `_word_offset` accept already-converted u16 column offsets.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CtdbRsError {
    InvalidNpar(usize),
    InvalidAudioLength { words: usize, stride: usize },
    InvalidParityLength { got: usize, need_at_least: usize },
    InvalidParityShape { stride: usize, npar: usize },
}
impl std::fmt::Display for CtdbRsError { /* … */ }
impl std::error::Error for CtdbRsError {}

pub mod galois {
    pub const FIELD_SIZE: usize = 65_536;
    pub const MAX: usize = FIELD_SIZE - 1;
    pub const PRIMITIVE_POLY: u32 = 0x1100B;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Galois16 { /* exp_tbl, log_tbl */ }

    impl Galois16 {
        pub fn new() -> Self;
        pub fn alpha_pow(&self, exp: usize) -> u16;
        pub fn mul(&self, a: u16, b: u16) -> u16;
        pub fn log(&self, value: u16) -> Option<usize>;
        pub fn exp_table(&self) -> &[u16];
        pub fn log_table(&self) -> &[u16];
        // ... (other GF accessors)
    }

    pub fn generate_tables() -> (Vec<u16>, Vec<u16>);
}

pub mod syndrome {
    use super::galois::Galois16;
    use super::CtdbRsError;

    /// 10 CD sectors × 588 stereo sample frames/sector × 2 u16 channels.
    pub const STRIDE: usize = 11_760;
    pub const DEFAULT_NPAR: usize = 8;

    pub fn i16_to_u16_bits(sample: i16) -> u16;
    pub fn u16_to_i16_bits(word: u16) -> i16;

    pub fn validate_npar(npar: usize) -> Result<(), CtdbRsError>;
    pub fn data_row_count(audio_words: usize) -> Result<usize, CtdbRsError>;

    pub fn make_generator_poly(gf: &Galois16, npar: usize) -> Result<Vec<u16>, CtdbRsError>;

    pub fn compute_column_parity(
        gf: &Galois16, gx: &[u16], column_data: &[u16], npar: usize,
    ) -> Vec<u16>;

    pub fn compute_parity_matrix_from_audio(
        gf: &Galois16, audio: &[i16], npar: usize,
    ) -> Result<Vec<Vec<u16>>, CtdbRsError>;

    pub fn compute_parity_matrix_from_u16_words(
        gf: &Galois16, words: &[u16], npar: usize,
    ) -> Result<Vec<Vec<u16>>, CtdbRsError>;

    /// Layout: data[(j + i*stride) * 2 .. + 2] = parity[j][i] as little-endian u16.
    /// Total bytes required: stride * npar * 2.
    pub fn try_bytes_to_parity(
        data: &[u8], stride: usize, npar: usize,
    ) -> Result<Vec<Vec<u16>>, CtdbRsError>;
    pub fn bytes_to_parity(data: &[u8], stride: usize, npar: usize) -> Vec<Vec<u16>>;

    pub fn parity_to_bytes(parity: &[Vec<u16>], stride: usize, npar: usize) -> Vec<u8>;

    /// Convert a parity matrix to a syndrome matrix, rotating columns by an
    /// already-converted u16-word offset.
    pub fn parity_to_syndrome_with_word_offset(
        gf: &Galois16, parity: &[Vec<u16>],
        npar: usize, stride: usize, word_offset: i64,
    ) -> Result<Vec<Vec<u16>>, CtdbRsError>;

    /// Convert a parity matrix to a syndrome matrix, treating `sample_offset`
    /// as a CD stereo sample-pair offset (multiplies internally by 2).
    pub fn parity_to_syndrome(
        gf: &Galois16, parity: &[Vec<u16>],
        npar: usize, stride: usize, sample_offset: i32,
    ) -> Result<Vec<Vec<u16>>, CtdbRsError>;

    pub fn xor_syndrome_matrices(lhs: &[Vec<u16>], rhs: &[Vec<u16>]) -> Vec<Vec<u16>>;
}

pub mod decoder {
    use super::galois::Galois16;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct LocatedError { pub row: usize, pub magnitude: u16 }

    pub fn all_zero(words: &[u16]) -> bool;

    pub fn berlekamp_massey(gf: &Galois16, syndromes: &[u16], npar: usize)
        -> Option<(Vec<u16>, usize)>;

    pub fn chien_search(gf: &Galois16, sigma: &[u16], errors_found: usize, stridecount: usize)
        -> Option<Vec<usize>>;

    pub fn syndrome_evaluator(gf: &Galois16, syndromes: &[u16], sigma: &[u16], npar: usize)
        -> Vec<u16>;

    pub fn forney(gf: &Galois16, syndromes: &[u16], sigma: &[u16],
                  positions: &[usize], npar: usize, stridecount: usize)
        -> Option<Vec<u16>>;

    pub fn residual_after_correction(/* ... */) -> Vec<u16>;
    pub fn decode_column_syndromes(/* ... */) -> /* ... */;
}

pub mod codec {
    use super::decoder::{self, LocatedError};
    use super::galois::Galois16;
    use super::syndrome::{self, STRIDE};
    use super::CtdbRsError;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct CtdbCodec { /* gf: Galois16 */ }

    impl Default for CtdbCodec { fn default() -> Self { Self::new() } }

    impl CtdbCodec {
        pub fn new() -> Self;
        pub fn galois(&self) -> &Galois16;

        /// Produces parity bytes laid out as `bytes_to_parity` expects.
        pub fn try_compute_parity(&self, audio: &[i16], npar: usize)
            -> Result<Vec<u8>, CtdbRsError>;
        pub fn compute_parity(&self, audio: &[i16], npar: usize) -> Vec<u8>;

        /// `audio` length should be a full disc image:
        ///   STRIDE leadin (11_760 i16 zeros) + concat(track i16 samples) + STRIDE leadout.
        /// `parity_bytes` is the entry's parity matrix as raw bytes
        /// (see `bytes_to_parity` for layout). `offset` is a CD stereo
        /// sample-pair offset (positive = drive read late).
        pub fn try_verify(
            &self, audio: &[i16], parity_bytes: &[u8],
            npar: usize, offset: i32,
        ) -> Result<VerifyResult, RepairError>;
        pub fn verify(&self, audio: &[i16], parity_bytes: &[u8],
                      npar: usize, offset: i32) -> VerifyResult;

        pub fn try_verify_with_sample_offset(
            &self, audio: &[i16], parity_bytes: &[u8],
            npar: usize, sample_offset: i32,
        ) -> Result<VerifyResult, RepairError>;

        /// `word_offset` is in u16 words = 2 × stereo sample pairs.
        pub fn try_verify_with_word_offset(
            &self, audio: &[i16], parity_bytes: &[u8],
            npar: usize, word_offset: i64,
        ) -> Result<VerifyResult, RepairError>;

        /// Repair audio in-place using parity bytes. Returns RepairResult
        /// or RepairError::Uncorrectable if any column has more errors
        /// than `npar / 2` (the RS error-correction limit).
        pub fn repair(&self, audio: &mut [i16], parity_bytes: &[u8],
                      npar: usize, offset: i32) -> Result<RepairResult, RepairError>;
        pub fn repair_with_sample_offset(/* … */) -> Result<RepairResult, RepairError>;
        pub fn repair_with_word_offset(/* … */) -> Result<RepairResult, RepairError>;
    }

    /// Result of RS verification. `matches == true` iff every column's
    /// syndrome XOR-difference is zero — i.e. the rip's audio (at the
    /// given offset) is byte-identical to the parity-source audio.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct VerifyResult {
        pub matches: bool,
        pub error_columns: usize,
        pub nonzero_syndromes: usize,
        /// Per-column syndrome bitwise-OR. 0 = column matches, nonzero = differs.
        pub column_magnitudes: Vec<u16>,
    }

    impl VerifyResult {
        pub fn from_syndromes(syndromes: &[Vec<u16>]) -> Self;
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct RepairResult {
        pub corrected_samples: usize,
        pub error_positions: Vec<(usize, usize)>,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum RepairError {
        Uncorrectable { column: usize, errors_found: usize, max_correctable: usize },
        InvalidParity(String),
        InvalidInput(String),
    }

    impl From<CtdbRsError> for RepairError { /* … */ }
    impl std::fmt::Display for RepairError { /* … */ }
    impl std::error::Error for RepairError {}
}

pub use codec::{CtdbCodec, RepairError, RepairResult, VerifyResult};
pub use decoder::{berlekamp_massey, chien_search, forney, LocatedError};
pub use galois::Galois16;
pub use syndrome::{bytes_to_parity, parity_to_bytes, DEFAULT_NPAR, STRIDE};
```

### Internal helper used by `try_verify_with_word_offset`

For context on what RS verification computes — this is the private method on `CtdbCodec` that `try_verify_*` calls (model can ignore unless needed):

```rust
fn syndrome_delta_with_word_offset(
    &self,
    audio: &[i16],
    parity_bytes: &[u8],
    npar: usize,
    word_offset: i64,
) -> Result<Vec<Vec<u16>>, RepairError> {
    syndrome::validate_npar(npar)?;
    let computed = syndrome::compute_parity_matrix_from_audio(&self.gf, audio, npar)?;
    let provided = syndrome::try_bytes_to_parity(parity_bytes, STRIDE, npar)?;

    let computed_syndrome = syndrome::parity_to_syndrome_with_word_offset(
        &self.gf, &computed, npar, STRIDE, 0,
    )?;
    let provided_syndrome = syndrome::parity_to_syndrome_with_word_offset(
        &self.gf, &provided, npar, STRIDE, word_offset,
    )?;

    // XOR difference: zero iff audio matches parity source.
    Ok(syndrome::xor_syndrome_matrices(&computed_syndrome, &provided_syndrome))
}
```

So `try_verify_*` returns `VerifyResult { matches: error_columns == 0, … }`. `matches` is the yes/no signal.

---

# 4. Source: `src/tui/cue_parser.rs` (excerpt)

```rust
// in src/tui/cue_parser.rs

/// Information about a single-image CUE album (one audio file + CUE sheet).
#[derive(Debug, Clone)]
pub struct SingleImageInfo {
    /// Path to the audio image file.
    pub audio_path: PathBuf,
    /// Path to the CUE sheet.
    pub cue_path: PathBuf,
    /// Parsed CUE sheet.
    pub sheet: CueSheet,
    /// Audio sample rate (e.g., 44100).
    pub sample_rate: u32,
    /// Total samples in the image file (stereo pair count).
    pub total_samples: u64,
    /// Per-track boundaries: (start_sample, sample_count), in stereo pairs.
    pub track_boundaries: Vec<(u64, u64)>,
}

/// Detect if `dir` contains a single-image CUE layout.
/// Returns Some if the directory has a CUE sheet with one FILE reference,
/// multiple TRACKs with INDEX 01 timestamps, and the referenced audio file
/// exists. Returns None for track-per-file layouts.
pub fn detect_single_image(dir: &Path) -> Option<SingleImageInfo>;
```

---

# 5. Source: `src/tui/app.rs` (excerpt)

```rust
// in src/tui/app.rs

/// State for the CUETools DB verification overlay.
/// Supports multi-disc: each page is one disc's results.
#[derive(Debug, Clone)]
pub struct CtdbVerifyState {
    pub pages: Vec<CtdbVerifyPage>,
    pub active_page: usize,
    pub scroll: usize,
}

/// A single page (disc) in the CTDB verification overlay.
#[derive(Debug, Clone)]
pub struct CtdbVerifyPage {
    pub label: String,
    pub result: crate::tui::ctdb::CtdbVerifyResult,
}

// ConfirmAction variants relevant to repair flow:
// (`Box<SingleImageInfo>` keeps the variant size small.)
pub enum ConfirmAction {
    // …
    CtdbRepair {
        paths: Vec<PathBuf>,
        parity_url: String,
        npar: usize,
        offset: i32,
        expected_crcs: Vec<u32>,
    },
    CtdbRepairSingleImage {
        info: Box<crate::tui::cue_parser::SingleImageInfo>,
        parity_url: String,
        npar: usize,
        offset: i32,
        expected_crcs: Vec<u32>,
    },
}
```

---

# 6. Source: `src/tui/command.rs` (excerpt — `Command::Ctdb` and `Command::CtdbRepair`)

```rust
// in src/tui/command.rs (verbatim, lines ~1691–1948)

Command::Ctdb => {
    // Same path collection as :ar.
    let mut paths: Vec<std::path::PathBuf> = match app.current_screen {
        AppScreen::Browse => {
            let sel = collect_selection_for_file_ops(app);
            super::browse::expand_paths_to_audio(&sel)
                .into_iter()
                .filter(|p| matches!(
                    super::browse::classify_file(p),
                    super::browse::EntryKind::AudioFile(_)
                ))
                .collect()
        }
        _ => Vec::new(),
    };
    super::probe::sort_paths_by_track(&mut paths);
    // Check for single-image CUE layout.
    if paths.len() <= 1 {
        let dir = if paths.is_empty() {
            let sel = collect_selection_for_file_ops(app);
            sel.first().and_then(|p| {
                if p.is_dir() { Some(p.clone()) } else { p.parent().map(|d| d.to_path_buf()) }
            })
        } else {
            paths[0].parent().map(|d| d.to_path_buf())
        };
        if let Some(ref dir) = dir {
            if let Some(info) = super::cue_parser::detect_single_image(dir) {
                let n = info.track_boundaries.len();
                let tx = tx.clone();
                app.set_status(format!(
                    "CUETools DB: verifying {} tracks (single image)...", n,
                ));
                tokio::spawn(async move {
                    let result = super::ctdb::verify_ctdb_single_image(&info).await;
                    let _ = tx.send(AppMessage::CtdbComplete {
                        pages: vec![super::app::CtdbVerifyPage {
                            label: String::new(),
                            result,
                        }],
                    }).await;
                });
                return;
            }
        }
    }
    if paths.is_empty() {
        app.set_status("No audio files for CTDB verification");
    } else {
        let groups = super::gnudb::group_by_disc(&paths);
        let n_groups = groups.len();
        let n_tracks: usize = groups.iter().map(|(_, p)| p.len()).sum();
        let tx = tx.clone();

        if n_groups <= 1 {
            // Single disc.
            let group_paths = groups.into_iter().next().unwrap().1;
            let sample_data = super::accuraterip::collect_sample_counts(&group_paths);
            match sample_data {
                Err(e) => {
                    app.set_status(format!("CTDB: {}", e));
                }
                Ok((sample_counts, sample_rate)) => {
                    app.set_status(format!(
                        "CUETools DB: verifying {} tracks...", n_tracks,
                    ));
                    tokio::spawn(async move {
                        let result = super::ctdb::verify_ctdb(
                            &group_paths, &sample_counts, sample_rate,
                        ).await;
                        let _ = tx.send(AppMessage::CtdbComplete {
                            pages: vec![super::app::CtdbVerifyPage {
                                label: String::new(),
                                result,
                            }],
                        }).await;
                    });
                }
            }
        } else {
            // Multi-disc — verify each disc sequentially.
            app.set_status(format!(
                "CUETools DB: verifying {} discs, {} tracks...",
                n_groups, n_tracks,
            ));
            tokio::spawn(async move {
                let mut pages = Vec::with_capacity(n_groups);
                for (idx, (label, mut group_paths)) in groups.into_iter().enumerate() {
                    let disc_name = if label.is_empty() {
                        format!("disc {}", idx + 1)
                    } else {
                        label.clone()
                    };
                    let _ = tx.send(AppMessage::StatusMessage(
                        format!("CUETools DB: verifying {}/{}  — {}...", idx + 1, n_groups, disc_name),
                    )).await;
                    super::probe::sort_paths_by_track(&mut group_paths);
                    let dir = group_paths[0]
                        .parent()
                        .unwrap_or(std::path::Path::new("."))
                        .to_path_buf();

                    // Per-disc single-image detection.
                    let result = if let Some(info) = super::cue_parser::detect_single_image(&dir) {
                        super::ctdb::verify_ctdb_single_image(&info).await
                    } else {
                        match super::accuraterip::collect_sample_counts(&group_paths) {
                            Ok((sample_counts, sample_rate)) => {
                                super::ctdb::verify_ctdb(
                                    &group_paths, &sample_counts, sample_rate,
                                ).await
                            }
                            Err(e) => {
                                log::warn!("CTDB: skipping disc '{}': {}", label, e);
                                continue;
                            }
                        }
                    };

                    pages.push(super::app::CtdbVerifyPage { label, result });
                }
                if !pages.is_empty() {
                    let _ = tx.send(AppMessage::CtdbComplete { pages }).await;
                }
            });
        }
    }
}

Command::CtdbRepair => {
    // If the CTDB overlay is open with parity available, extract repair
    // parameters from it. Otherwise, run CTDB verify first.
    if let ActiveOverlay::CtdbVerify(ref state) = app.active_overlay {
        let page = &state.pages[state.active_page];
        let result = &page.result;

        // Check that parity is available.
        let parity_url = match &result.parity_url {
            Some(url) => url.clone(),
            None => {
                app.set_status("No parity data available for this disc");
                return;
            }
        };

        let npar = match result.npar {
            Some(n) => n as usize,
            None => {
                app.set_status("CTDB entry missing npar value");
                return;
            }
        };

        // Check if any tracks have mismatches.
        let has_mismatch = result.tracks.iter().any(|t| {
            t.status == super::ctdb::CtdbTrackStatus::Mismatch
        });
        if !has_mismatch {
            app.set_status("No mismatches detected — repair not needed");
            return;
        }

        let paths: Vec<std::path::PathBuf> = result.tracks.iter()
            .map(|t| t.path.clone())
            .collect();
        let n = paths.len();

        // Extract expected CRCs from the CTDB entry for post-repair
        // verification. These come from the database, not our computation.
        let expected_crcs: Vec<u32> = result.tracks.iter()
            .filter_map(|t| t.expected_crc32)
            .collect();
        if expected_crcs.len() != n {
            app.set_status("Cannot repair: missing expected CRC for some tracks");
            return;
        }

        // Detect single-image CUE layout: all tracks point at the same file.
        let single_image: Option<Box<super::cue_parser::SingleImageInfo>> =
            if n > 1 && paths.iter().all(|p| p == &paths[0]) {
                let dir = paths[0].parent().unwrap_or(std::path::Path::new("."));
                match super::cue_parser::detect_single_image(dir) {
                    Some(info) => Some(Box::new(info)),
                    None => {
                        app.set_status("Single-image CTDB repair: failed to detect CUE layout");
                        return;
                    }
                }
            } else {
                None
            };

        let cache_query_paths: Vec<std::path::PathBuf> = if single_image.is_some() {
            vec![paths[0].clone()]
        } else {
            paths.clone()
        };

        // Auto-detect drive read offset from AR cache. None means defer to AR.
        match detect_ar_offset_from_cache(&app.db, &cache_query_paths) {
            Some(offset) => {
                let offset_note = if offset != 0 {
                    format!("offset: {:+} samples (from AR cache)", offset)
                } else {
                    "offset: +0 (verified by AR)".to_string()
                };
                let message = format!(
                    "Apply CTDB Reed-Solomon repair to {} tracks?\n\
                     Parity: {} symbols, {}\n\
                     Files will be re-encoded and verified before replacing originals.",
                    n, npar, offset_note,
                );
                let action = match single_image {
                    Some(info) => super::app::ConfirmAction::CtdbRepairSingleImage {
                        info, parity_url, npar, offset, expected_crcs,
                    },
                    None => super::app::ConfirmAction::CtdbRepair {
                        paths, parity_url, npar, offset, expected_crcs,
                    },
                };
                app.active_overlay = ActiveOverlay::Confirmation { message, action };
            }
            None => {
                // No usable AR cache — defer until AR completes.
                app.pending_ctdb_repair = Some(super::app::PendingCtdbRepair {
                    paths, parity_url, npar, expected_crcs, single_image,
                });
                app.set_status(
                    "No AR offset cached — running AccurateRip to detect drive offset...",
                );
                execute_command(app, Command::AccurateRip { force: false }, tx);
            }
        }
    } else {
        // No CTDB overlay open. Run CTDB verify first; the auto-repair
        // flag tells the CtdbComplete handler to re-dispatch :ctdb-repair
        // once the verification overlay is installed.
        app.auto_repair_on_ctdb_complete = true;
        app.set_status(
            "Running CUETools DB verification first to detect mismatches...",
        );
        execute_command(app, Command::Ctdb, tx);
    }
}
```

---

## End of bundle

Hand this single file to the reasoning model. The model has everything it needs to (a) confirm/correct the diagnosis, (b) design the verification flow, (c) generate Rust code that integrates with our existing `CtdbCodec` API.
